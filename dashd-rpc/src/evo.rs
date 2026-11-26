use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_dml::mnlistdiff::{QuorumClSig, QuorumKey};
use dash_dml::store::StoreError;
use dash_dml::{Dmn, MnList, MnStateDiff, ProTxHash};
use dash_evo::sml::SmlEntry;
use dash_num::Hash256;
use dash_primitives::{BlockHash, Commitment};

use crate::UniValue;
use crate::core_write::TxJsonParams;
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RpcError};
use crate::request::JsonRpcRequest;
use crate::server::{Command, NodeControl, RpcServer};

mod diff;
mod protx;
mod quorum;
mod recsig;
mod sign;

/// Range checks and the null-hash genesis alias are resolved by the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRef {
    Hash(BlockHash),
    Height(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockArg {
    pub name: &'static str,
    pub value_str: String,
    pub syntax: Result<BlockRef, RpcError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockFailure {
    BadArg,
    NotFound(BlockHash),
}

impl BlockArg {
    pub fn failure(&self, failure: BlockFailure) -> EvoError {
        EvoError::Block {
            name: self.name,
            value_str: self.value_str.clone(),
            failure,
        }
    }

    pub fn hash(
        &self,
        tip: i32,
        hash_at_height: impl FnOnce(i32) -> Option<BlockHash>,
    ) -> Result<BlockHash, EvoError> {
        match self.syntax.clone().map_err(EvoError::Param)? {
            BlockRef::Hash(hash) => Ok(hash),
            BlockRef::Height(height) if (1..=tip).contains(&height) => {
                hash_at_height(height).ok_or_else(|| self.failure(BlockFailure::BadArg))
            }
            BlockRef::Height(_) => Err(self.failure(BlockFailure::BadArg)),
        }
    }
}

/// `protx list`'s type argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MnListType {
    Registered,
    /// Not PoSe-banned (`IsMNValid`).
    Valid,
    Evo,
}

impl MnListType {
    pub fn includes(self, dmn: &Dmn) -> bool {
        match self {
            MnListType::Registered => true,
            MnListType::Valid => dmn.is_valid(),
            MnListType::Evo => dmn.is_evo(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MnListRequest {
    /// `protx list` at a height of the active chain, the tip by default.
    Height {
        height: Option<i32>,
        mn_type: MnListType,
        detailed: bool,
    },
    /// `protx info` at a block, the tip by default.
    Info {
        pro_tx_hash: ProTxHash,
        block: Option<BlockHash>,
    },
}

impl MnListRequest {
    /// The entries `BuildDMNListEntry` prints in full, the only ones whose collateral and meta
    /// info Core reads: a non-detailed list reads none, and `info` reads its one masternode.
    // Core: rpc/evo.cpp:1595-1597, :1735-1747, :1834-1836
    pub fn detailed_entries<'a>(&self, list: &'a MnList) -> Vec<&'a Arc<Dmn>> {
        match *self {
            MnListRequest::Height {
                mn_type,
                detailed: true,
                ..
            } => list.iter().filter(|dmn| mn_type.includes(dmn)).collect(),
            MnListRequest::Height {
                detailed: false,
                ..
            } => Vec::new(),
            MnListRequest::Info {
                pro_tx_hash,
                ..
            } => list.get(&pro_tx_hash).into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MnListAt {
    pub height: i32,
    pub block: BlockHash,
    pub list: Arc<MnList>,
    pub tip_height: i32,
    /// Raw confirmations at the current tip of each of the request's
    /// [`MnListRequest::detailed_entries`]; -1 for spent or unknown collateral.
    pub confirmations: BTreeMap<ProTxHash, i32>,
    /// Meta info of the same entries.
    pub meta: BTreeMap<ProTxHash, MetaFacts>,
    pub now: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaFacts {
    pub last_dsq: i64,
    pub mixing_tx_count: i32,
    pub outbound_attempt_count: i32,
    pub last_outbound_attempt: i64,
    pub last_outbound_success: i64,
    pub is_platform_banned: bool,
    pub platform_ban_height_updated: i32,
}

/// Added, removed and updated entries retain internal-id order.
#[derive(Clone, Debug)]
pub struct MnListDiffFacts {
    pub base_height: i32,
    pub block_height: i32,
    pub added: Vec<Arc<Dmn>>,
    pub removed: Vec<ProTxHash>,
    /// Target states paired with their diffs, in internal-id order.
    pub updated: Vec<(Arc<Dmn>, MnStateDiff)>,
}

#[derive(Clone, Debug)]
pub struct SmlEntryFacts {
    pub entry: SmlEntry,
    pub dmn: Arc<Dmn>,
}

#[derive(Clone, Debug)]
pub struct SmlDiffFacts {
    pub base_hash_echo: BlockHash,
    /// Target list hash, which is null before DIP3.
    pub block_hash: BlockHash,
    pub cbtx_hex: String,
    pub merkle_tree_hex: String,
    pub deleted: Vec<ProTxHash>,
    pub entries: Vec<SmlEntryFacts>,
    pub deleted_quorums: Vec<QuorumKey>,
    pub new_quorums: Vec<Commitment>,
    pub roots: Option<(Hash256, Option<Hash256>)>,
    pub cl_sigs: Vec<QuorumClSig>,
    pub bls: BlsCtx,
}

pub type QuorumMemberFacts = (Arc<Dmn>, bool);

#[derive(Clone, Debug)]
pub struct QuorumFacts {
    pub type_name: String,
    pub rotated: bool,
    pub base_height: i32,
    pub hash: BlockHash,
    pub index: i16,
    pub mined_block: BlockHash,
    pub prev_failures: Option<i32>,
    pub members: Vec<QuorumMemberFacts>,
    /// Valid bits among the actual members; members.len() is the health denominator.
    pub valid_count: usize,
    pub single_member: bool,
    pub bls: BlsCtx,
    /// Public key bytes serialized under bls.
    pub quorum_pk: [u8; 48],
    /// Per member, Core's `GetPubKeyShare(i)` under bls where it is valid; empty for a
    /// single-member type or a quorum read for a list.
    pub pub_key_shares: Vec<Option<[u8; 48]>>,
    /// Core's `GetSkShare()`, only when `includeSkShare` asked for it and the share is valid.
    pub sk_share: Option<[u8; 32]>,
}

#[derive(Clone, Debug)]
pub struct QuorumListFacts {
    pub type_name: String,
    pub quorums: Vec<QuorumFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvoError {
    Unavailable,
    LlmqUnavailable,
    NotInMainChain,
    InvalidHeight,
    HeightOutOfRange,
    MasternodeNotFound(ProTxHash),
    InvalidQuorumType,
    QuorumNotFound,
    ListUnavailable(String),
    Store(String),
    Block {
        name: &'static str,
        value_str: String,
        failure: BlockFailure,
    },
    Param(RpcError),
    Failed(String),
}

impl EvoError {
    /// Maps shared errors; block failures keep the handler's RPC-specific policy.
    pub fn into_rpc(self, block_error: impl FnOnce(Self) -> RpcError) -> RpcError {
        match self {
            EvoError::Unavailable => {
                RpcError::new(RPC_INTERNAL_ERROR, "Node evo context unavailable")
            }
            EvoError::LlmqUnavailable => {
                RpcError::new(RPC_INTERNAL_ERROR, "Node LLMQ context not found")
            }
            error @ EvoError::Block {
                ..
            } => block_error(error),
            EvoError::Param(error) => error,
            EvoError::NotInMainChain => {
                RpcError::new(RPC_INVALID_PARAMETER, "Block is not in main chain")
            }
            EvoError::InvalidHeight => {
                RpcError::new(RPC_INVALID_PARAMETER, "invalid height specified")
            }
            EvoError::HeightOutOfRange => {
                RpcError::new(RPC_INVALID_PARAMETER, "Block height out of range")
            }
            EvoError::MasternodeNotFound(hash) => {
                RpcError::new(RPC_INVALID_PARAMETER, format!("{hash} not found"))
            }
            EvoError::InvalidQuorumType => {
                RpcError::new(RPC_INVALID_PARAMETER, "invalid LLMQ type")
            }
            EvoError::QuorumNotFound => RpcError::new(RPC_INVALID_PARAMETER, "quorum not found"),
            EvoError::ListUnavailable(message)
            | EvoError::Store(message)
            | EvoError::Failed(message) => RpcError::Runtime(message),
        }
    }
}

impl From<StoreError> for EvoError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotBuilt(_) => Self::Unavailable,
            StoreError::ListUnavailable(_) => Self::ListUnavailable(error.to_string()),
            StoreError::Read {
                ..
            }
            | StoreError::Inconsistent {
                ..
            } => Self::Store(error.to_string()),
        }
    }
}

pub trait NodeEvo: Send + Sync {
    fn permissive_bool(&self) -> bool;
    fn mn_list(&self, request: MnListRequest) -> Result<MnListAt, EvoError>;
    fn mn_list_diff(&self, base: BlockArg, block: BlockArg) -> Result<MnListDiffFacts, EvoError>;
    fn sml_diff(
        &self,
        base: BlockArg,
        block: BlockArg,
        extended: Result<bool, RpcError>,
    ) -> Result<SmlDiffFacts, EvoError>;
    fn quorum_list(&self, count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError>;
    fn quorum_list_extended(&self, height: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError>;
    fn llmq_type_known(&self, llmq_type: u8) -> bool;
    fn quorum_info(
        &self,
        llmq_type: u8,
        hash: BlockHash,
        include_sk_share: bool,
    ) -> Result<QuorumFacts, EvoError>;
    /// `quorum dkgstatus`'s answer at `detail` (0, 1 or 2): the debug manager's sessions, the
    /// quorum connections and the mineable commitments. A node that serves none has no chain.
    fn dkg_status(&self, detail: i32) -> Result<UniValue, EvoError> {
        let _ = detail;
        Err(EvoError::Unavailable)
    }
    /// `quorum memberof`'s reads in one turn at the tip: `None` when the tip list lacks
    /// `pro_tx_hash`, else [`quorum_list`](Self::quorum_list)'s scan with each rotated quorum's
    /// `previousConsecutiveDKGFailures`.
    fn quorum_member_of(
        &self,
        pro_tx_hash: ProTxHash,
        count: Option<i32>,
    ) -> Result<Option<Vec<QuorumListFacts>>, EvoError> {
        let _ = (pro_tx_hash, count);
        Err(EvoError::Unavailable)
    }
}

pub fn register_evo_commands(server: &RpcServer, evo: Arc<dyn NodeEvo>, p: TxJsonParams) {
    for name in ["protx", "quorum"] {
        server.append_command(Command::new(name, |_| {
            Err(RpcError::new(RPC_INVALID_PARAMETER, "Must be a valid command"))
        }));
    }
    type Handler = fn(&JsonRpcRequest, &dyn NodeEvo, &TxJsonParams) -> Result<UniValue, RpcError>;
    for (name, handler) in [
        ("quorum list", quorum::list as Handler),
        ("quorum listextended", quorum::list_extended as Handler),
        ("quorum info", quorum::info as Handler),
        ("quorum dkgstatus", quorum::dkg_status as Handler),
        ("quorum memberof", quorum::member_of as Handler),
    ] {
        let evo = Arc::clone(&evo);
        let p = p.clone();
        server
            .append_command(Command::new(name, move |request| handler(request, evo.as_ref(), &p)));
    }
    // A failed `CHECK_NONFATAL` answers with the node's identity, which needs the control.
    type ControlHandler = fn(
        &JsonRpcRequest,
        &dyn NodeEvo,
        &TxJsonParams,
        &dyn NodeControl,
    ) -> Result<UniValue, RpcError>;
    for (name, handler) in [
        ("protx list", protx::list as ControlHandler),
        ("protx info", protx::info as ControlHandler),
        ("protx listdiff", protx::list_diff as ControlHandler),
        ("protx diff", diff::diff as ControlHandler),
    ] {
        let evo = Arc::clone(&evo);
        let p = p.clone();
        let control = Arc::clone(server.control());
        server.append_command(Command::new(name, move |request| {
            handler(request, evo.as_ref(), &p, control.as_ref())
        }));
    }
}

pub(crate) use protx::info_error;
pub use recsig::NodeRecSigs;

/// `quorum hasrecsig`, `getrecsig` and `isconflicting`, over the node's evo reads and its signing
/// manager.
pub fn register_recsig_commands(
    server: &RpcServer,
    evo: Arc<dyn NodeEvo>,
    sigs: Arc<dyn NodeRecSigs>,
) {
    for (name, handler) in [
        ("quorum hasrecsig", recsig::has as recsig::Handler),
        ("quorum getrecsig", recsig::get as recsig::Handler),
        ("quorum isconflicting", recsig::is_conflicting as recsig::Handler),
    ] {
        let (evo, sigs) = (Arc::clone(&evo), Arc::clone(&sigs));
        server.append_command(Command::new(name, move |request| {
            handler(request, evo.as_ref(), sigs.as_ref())
        }));
    }
}

pub use sign::{NodeQuorumSign, OwnSigShare, SelectedQuorum, SignArgs, VerifyArgs, VerifyAt};

/// `quorum sign`, `platformsign`, `selectquorum` and `verify`, over the node's evo reads and its
/// share manager.
pub fn register_sign_commands(
    server: &RpcServer,
    evo: Arc<dyn NodeEvo>,
    node: Arc<dyn NodeQuorumSign>,
) {
    for (name, handler) in [
        ("quorum sign", sign::sign as sign::Handler),
        ("quorum platformsign", sign::platform_sign as sign::Handler),
        ("quorum selectquorum", sign::select_quorum as sign::Handler),
        ("quorum verify", sign::verify as sign::Handler),
    ] {
        let (evo, node) = (Arc::clone(&evo), Arc::clone(&node));
        server.append_command(Command::new(name, move |request| {
            handler(request, evo.as_ref(), node.as_ref())
        }));
    }
}

#[cfg(test)]
mod tests;
