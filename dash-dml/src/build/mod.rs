//! `build_new_list_from_block`: Core's `CSpecialTxProcessor::RebuildListFromBlock`
//! (`specialtxman.cpp` v24:309-795), step by step in Core's order (EV2 §3.2, quirk 25).
//!
//! The parity reference is v24.0.0-rc.3 alone (DEC-82): its order is ported verbatim, including
//! the ProUpServTx type check before the NetInfo loop (D1), with every V24 branch keyed by
//! [`DmlRules::v24`].

mod state;

use std::fmt;
use std::sync::Arc;

use dash_consensus::MAX_MONEY;
use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::{Block, Tx};
use dash_consensus::subsidy::COIN;
use dash_consensus::{BlockId, BlockValidationError, BlockValidationResult, ChainAncestry};
use dash_evo::effect::EffectSink;
use dash_num::Hash256;
use dash_params::{ChainParams, LlmqParams};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{
    BlockHash, FinalCommitment, LlmqType, MnType as PayloadMnType, NetInfo, OutPoint, ProRegTx,
    ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx, SpecialPayload, TxHash,
    TxType,
};
use dash_types::Numeric;

use crate::list::{MnList, MnListError};
use crate::netinfo::NetInfoRules;
use crate::payee::PayeeRules;
use crate::pose::phase_order;
use crate::scan::ScanList;
use crate::specialtx::reasons::{
    BAD_PROTX_COLLATERAL, BAD_PROTX_DUP_KEY, BAD_PROTX_HASH, BAD_PROTX_NETINFO_ENTRY,
    BAD_PROTX_PAYLOAD, BAD_PROTX_SHARED_MN, BAD_PROTX_TYPE, BAD_PROTX_TYPE_MISMATCH,
    BAD_PROUPSHARE_INDEX, BAD_PROUPSHARE_PAYEE_REUSE, BAD_PROUPSHAREDREG_NOT_SHARED,
    BAD_PROUPSHAREDREG_PAYEE_REUSE, BAD_SHARED_COLLATERAL_SPEND,
};
use crate::store::{DmlMemberSource, NodeMemberError, StoreError};
use crate::trace::{DmlEvent, DmlTrace};
use crate::types::{
    Dmn, EXT_ADDR, MnCold, MnHot, OperatorKey, OperatorPoint, ext_addr, is_default_operator_key,
    scheme_for,
};
use crate::unique::{NetProp, UniqueKey, net_props};
use crate::{ProTxHash, RawMnType};

use state::{from_pro_reg, legacy_payout_as_list, share_list_voting_key_safe};

/// `bad-protx-dup-netinfo-entry`.
const BAD_PROTX_DUP_NETINFO_ENTRY: &str = "bad-protx-dup-netinfo-entry";
/// `bad-qc-payload`.
const BAD_QC_PAYLOAD: &str = "bad-qc-payload";
/// `bad-qc-commitment-type`.
const BAD_QC_COMMITMENT_TYPE: &str = "bad-qc-commitment-type";
/// `bad-qc-quorum-hash`.
const BAD_QC_QUORUM_HASH: &str = "bad-qc-quorum-hash";
/// Not a Core string: the [`StoreError::Inconsistent`] of a predecessor list that is neither the
/// default list nor the list of `pindexPrev`, where Core asserts (`specialtxman.cpp` v24:317). A
/// node fault, never a block verdict (EV2b D8); unreachable from the chainstate.
pub const DML_PREV_LIST_MISMATCH: &str = "dml-prev-list-mismatch";
/// `failed-procspectxsinblock`: what the chainstate maps a [`DmlBuildError::Exception`] to.
pub const FAILED_PROCSPECTXSINBLOCK: &str = "failed-procspectxsinblock";

/// The chain parameters the build and `process_block` read (from `ChainParams`, BS5).
#[derive(Clone, Debug)]
pub struct DmlParams {
    /// `nMasternodeMinimumConfirmations`: mainnet 15, every other network 1.
    pub min_confirmations: i32,
    /// `DIP0003Height`.
    pub dip3_height: i32,
    /// `DIP0003EnforcementHeight`.
    pub dip3_enforcement_height: i32,
    /// `DIP0003EnforcementHash`, `None` when null (quirk 34's pin).
    pub dip3_enforcement_hash: Option<BlockHash>,
    /// Every configured LLMQ type (`GetLLMQ`).
    pub llmqs: Vec<LlmqParams>,
}

impl DmlParams {
    /// The values of `params`.
    pub fn new(params: &ChainParams) -> DmlParams {
        let c = &params.consensus;
        let (enforcement_height, enforcement_hash) = c.dip0003_enforcement;
        DmlParams {
            min_confirmations: c.masternode_minimum_confirmations,
            dip3_height: c.dip0003_height.to_u32() as i32,
            dip3_enforcement_height: enforcement_height.to_u32() as i32,
            dip3_enforcement_hash: (!enforcement_hash.is_null())
                .then(|| BlockHash::from(*enforcement_hash.as_bytes())),
            llmqs: c.llmqs.to_vec(),
        }
    }

    /// `Params().GetLLMQ(type)`.
    pub fn llmq(&self, llmq_type: LlmqType) -> Option<&LlmqParams> {
        self.llmqs.iter().find(|q| q.llmq_type == llmq_type)
    }
}

/// The rules one build follows: the chain's parameters and NetInfo rules, and the deployments
/// Core reads at `pindexPrev`.
#[derive(Clone, Copy, Debug)]
pub struct DmlRules<'a> {
    pub params: &'a DmlParams,
    /// `SetStateVersion`'s NetInfo rules (network-dependent).
    pub net_info: &'a NetInfoRules,
    /// `DeploymentActiveAfter(pindexPrev, V19)`.
    pub v19: bool,
    /// `DeploymentActiveAfter(pindexPrev, MN_RR)`.
    pub mn_rr: bool,
    /// `GetSpecialTxRules(pindexPrev).v24` (`validation.cpp` v24:2659).
    pub v24: bool,
}

impl DmlRules<'_> {
    /// The payee rules at `pindexPrev`.
    pub fn payee(&self) -> PayeeRules {
        PayeeRules {
            v19: self.v19,
            mn_rr: self.mn_rr,
        }
    }
}

/// `pindexPrev`.
#[derive(Clone, Copy)]
pub struct PrevBlock<'a> {
    pub id: BlockId,
    pub hash: BlockHash,
    pub height: i32,
    pub chain: &'a dyn ChainAncestry,
}

/// A coins view that holds nothing and has no best block: Core's null-best-block path, where
/// external collateral is not re-checked (`specialtxman.cpp` v24:378; evodb repair, the L2
/// harness).
pub struct NoCoins;

impl CoinsView for NoCoins {
    type Error = std::convert::Infallible;

    fn coin(
        &self,
        _: &OutPoint,
    ) -> Result<Option<dash_consensus::coins::CoinRef<'_>>, Self::Error> {
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// The result of a build: the new list (block hash null, as Core leaves it) and the payee the
/// predecessor list selected, if any (EV3 reads it). `L` is the reference twin's list for its
/// build (EV2b D24).
pub struct BuiltList<L = MnList> {
    pub list: L,
    pub payee: Option<Arc<Dmn>>,
}

/// Why a build failed (EV2 §2.11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmlBuildError {
    /// Core's explicit `state.Invalid(BLOCK_CONSENSUS, reason)`.
    Invalid(BlockValidationError),
    /// A Core `std::runtime_error`: the chainstate maps it to `failed-procspectxsinblock`.
    Exception(DmlException),
    /// A local store fault: a node error, never a block verdict (EV addendum §2.3).
    Store(StoreError),
}

impl DmlBuildError {
    fn invalid(reason: &'static str) -> Self {
        DmlBuildError::Invalid(BlockValidationError::new(BlockValidationResult::Consensus, reason))
    }

    /// The block's reject reason as the chainstate reports it: the explicit reason, or
    /// `failed-procspectxsinblock` for an exception. `None` for a store fault.
    pub fn reject_reason(&self) -> Option<&str> {
        match self {
            DmlBuildError::Invalid(e) => Some(e.reject_reason()),
            DmlBuildError::Exception(_) => Some(FAILED_PROCSPECTXSINBLOCK),
            DmlBuildError::Store(_) => None,
        }
    }
}

impl fmt::Display for DmlBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmlBuildError::Invalid(e) => write!(f, "{e}"),
            DmlBuildError::Exception(e) => write!(f, "{FAILED_PROCSPECTXSINBLOCK}: {e}"),
            DmlBuildError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DmlBuildError {}

impl From<MnListError> for DmlBuildError {
    fn from(e: MnListError) -> Self {
        DmlBuildError::Exception(DmlException::List(e))
    }
}

/// A Core exception inside the build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmlException {
    /// An `AddMN`/`UpdateMN`/`RemoveMN`/`PoSePunish` throw.
    List(MnListError),
    /// `IsQuorumTypeEnabled`'s throw for an unknown type; unreachable (the type is checked
    /// first).
    Members(u8),
    /// Input on which Core has undefined behaviour or that a validated chain never holds; never a
    /// panic (EV2 §3.11 item 2; D2).
    OutOfDomain(&'static str),
}

impl fmt::Display for DmlException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmlException::List(e) => f.write_str(&e.core_message()),
            DmlException::Members(t) => write!(f, "unknown LLMQ type {t}"),
            DmlException::OutOfDomain(what) => write!(f, "out of domain: {what}"),
        }
    }
}

/// Core's `GetTxPayload<T>(tx)`: the decoded payload when the decoder read every byte.
// Core: evo/specialtx.h:17-28
pub fn payload(tx: &Tx) -> Option<&SpecialPayload> {
    match tx.payload() {
        Some(Ok(decoded)) if decoded.is_complete() => Some(&decoded.payload),
        _ => None,
    }
}

/// The version of a provider payload (types 1 to 4) of which Core reads only the version: 0, or
/// above every known one (D2; base-sdk's `UnknownProTxVersion`, EV1 D20). `GetTxPayload` reads
/// it when it is the whole payload, and Core's check loop rejects it, `bad-protx-version`
/// (`GetValidatedPayload`'s maximum, or `IsTriviallyValid`'s version 0). The build refuses it as
/// out of domain; EV2b's check-layer scaffolding (`node-l1` `dml_checks`, D56) shares this test.
pub fn unknown_pro_tx_version(tx: &Tx) -> Option<u16> {
    match payload(tx) {
        Some(SpecialPayload::UnknownProTxVersion {
            version,
            ..
        }) => Some(*version),
        _ => None,
    }
}

/// A provider transaction's `proTxHash` as the list's key type.
pub(crate) fn pro_tx_of(h: &TxHash) -> ProTxHash {
    ProTxHash::from(*h.as_bytes())
}

/// The masternode `pro_tx` of the list, or `bad-protx-hash`.
fn existing(new: &impl BuildList, pro_tx: &ProTxHash) -> Result<Arc<Dmn>, DmlBuildError> {
    new.get(pro_tx).map(Arc::clone).ok_or_else(|| DmlBuildError::invalid(BAD_PROTX_HASH))
}

/// A payload of a known provider type whose version Core reads but this build cannot (D2).
fn out_of_domain_version() -> DmlBuildError {
    DmlBuildError::Exception(DmlException::OutOfDomain("provider payload of an unknown version"))
}

/// The list a build mutates (EV2b D24): [`MnList`], whose bulk phases find their candidates
/// through its indexes, and the reference twin's list under xcheck, which runs Core's literal
/// scans over its own layout. The special transactions, the collateral spends and the payee
/// bookkeeping are one generic port (checked against Core by `diff_dml_apply`); the phases and
/// the list operations below are each type's own.
pub(crate) trait BuildList: ScanList + Clone {
    /// `GetBlockHash()`.
    fn block_hash(&self) -> &BlockHash;
    /// Whether this is Core's default `CDeterministicMNList()`.
    fn is_default(&self) -> bool;
    /// `prevList` with `SetBlockHash(uint256())` and `SetHeight(height)` (v24:319-321).
    fn start_build(&self, height: i32) -> Self;
    /// `GetTotalRegisteredCount()`.
    fn total_registered(&self) -> u32;
    /// `HasUniqueProperty(v)`.
    fn has_unique(&self, key: &UniqueKey) -> bool;
    /// `GetUniquePropertyMN(v)`.
    fn unique_owner(&self, key: &UniqueKey) -> Option<&Arc<Dmn>>;
    /// `AddMN(dmn, fBumpTotalCount)`.
    fn try_add(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError>;
    /// `UpdateMN(proTxHash, newState)`.
    fn update(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
        cold: Arc<MnCold>,
    ) -> Result<(), MnListError>;
    /// `UpdateMN` of a state whose cold fields are the masternode's own.
    fn update_hot(&mut self, pro_tx: &ProTxHash, hot: MnHot) -> Result<(), MnListError>;
    /// `RemoveMN(proTxHash)`.
    fn remove(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError>;
    /// `CalcPenalty(percent)`.
    fn calc_penalty(&self, percent: i32) -> i32;
    /// `PoSePunish(proTxHash, penalty)`.
    fn pose_punish(
        &mut self,
        pro_tx: &ProTxHash,
        penalty: i32,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError>;
    /// `GetMNPayee(pindexPrev)`.
    fn payee(&self, rules: PayeeRules) -> Option<Arc<Dmn>>;
    /// Step 1, the confirmations over `prev` (v24:330-343).
    fn confirm(
        &mut self,
        prev: &Self,
        prev_height: i32,
        min_conf: i32,
        confirmed: &Hash256,
    ) -> Result<(), MnListError>;
    /// `DecreaseScores()`.
    fn decrease_scores(&mut self) -> Result<(), MnListError>;
    /// Step 6, the streak reset of every unpaid evonode (v24:777-790).
    fn reset_streaks(
        &mut self,
        payee: Option<ProTxHash>,
        mn_rr: bool,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError>;

    /// `GetMNByCollateral(collateralOutpoint)` (v24:104-107).
    fn get_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>> {
        self.unique_owner(&UniqueKey::collateral(op))
    }

    /// `HasOperatorKeyUnderAnyScheme(pubkey, self)` (`deterministicmns.h` v24:471-482).
    // Core: evo/deterministicmns.h:471 CDeterministicMNList::HasOperatorKeyUnderAnyScheme
    fn has_operator_key_under_any_scheme(&self, key: &OperatorPoint, except: &ProTxHash) -> bool {
        if !key.is_valid() {
            return false;
        }
        [BlsSchemeKind::Legacy, BlsSchemeKind::Basic].into_iter().any(|scheme| {
            self.unique_owner(&UniqueKey::operator_point(key, scheme))
                .is_some_and(|holder| holder.pro_tx_hash != *except)
        })
    }
}

impl BuildList for MnList {
    fn block_hash(&self) -> &BlockHash {
        MnList::block_hash(self)
    }

    fn is_default(&self) -> bool {
        MnList::is_default(self)
    }

    fn start_build(&self, height: i32) -> Self {
        self.clone().with_block_hash(BlockHash::from([0; 32])).with_height(height)
    }

    fn total_registered(&self) -> u32 {
        MnList::total_registered(self)
    }

    fn has_unique(&self, key: &UniqueKey) -> bool {
        MnList::has_unique(self, key)
    }

    fn unique_owner(&self, key: &UniqueKey) -> Option<&Arc<Dmn>> {
        MnList::unique_owner(self, key)
    }

    fn try_add(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError> {
        MnList::add_mn(self, dmn, bump_total)
    }

    fn update(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
        cold: Arc<MnCold>,
    ) -> Result<(), MnListError> {
        MnList::update_mn(self, pro_tx, hot, cold)
    }

    fn update_hot(&mut self, pro_tx: &ProTxHash, hot: MnHot) -> Result<(), MnListError> {
        MnList::update_hot_mn(self, pro_tx, hot)
    }

    fn remove(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError> {
        MnList::remove_mn(self, pro_tx)
    }

    fn calc_penalty(&self, percent: i32) -> i32 {
        MnList::calc_penalty(self, percent)
    }

    fn pose_punish(
        &mut self,
        pro_tx: &ProTxHash,
        penalty: i32,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError> {
        MnList::pose_punish(self, pro_tx, penalty, trace)
    }

    fn payee(&self, rules: PayeeRules) -> Option<Arc<Dmn>> {
        MnList::payee(self, rules).cloned()
    }

    /// Candidates come from the unconfirmed index; Core's `pindexPrev->nHeight -
    /// nRegisteredHeight` is an `int` subtraction (wrapping here), which only excludes candidates
    /// the `i64` bound admits.
    fn confirm(
        &mut self,
        prev: &Self,
        prev_height: i32,
        min_conf: i32,
        confirmed: &Hash256,
    ) -> Result<(), MnListError> {
        let bound = i64::from(prev_height) - i64::from(min_conf);
        let candidates: Vec<ProTxHash> = prev
            .unconfirmed()
            .take_while(|(registered, _)| i64::from(*registered) <= bound)
            .filter(|(registered, _)| prev_height.wrapping_sub(*registered) >= min_conf)
            .map(|(_, pro_tx)| *pro_tx)
            .collect();
        for pro_tx in phase_order(prev, candidates) {
            let dmn =
                MnList::get(self, &pro_tx).ok_or(MnListError::NotFound("UpdateMN", pro_tx))?;
            let mut cold = (*dmn.cold).clone();
            cold.update_confirmed_hash(&pro_tx, confirmed);
            let hot = dmn.hot;
            MnList::update_mn(self, &pro_tx, hot, Arc::new(cold))?;
        }
        Ok(())
    }

    fn decrease_scores(&mut self) -> Result<(), MnListError> {
        MnList::decrease_scores(self)
    }

    /// Candidates come from the evo-streak index.
    fn reset_streaks(
        &mut self,
        payee: Option<ProTxHash>,
        mn_rr: bool,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError> {
        let candidates: Vec<ProTxHash> =
            self.evo_streaks().filter(|pro_tx| mn_rr || Some(**pro_tx) != payee).copied().collect();
        for pro_tx in phase_order(self, candidates) {
            let from = MnList::get(self, &pro_tx).map_or(0, |d| d.hot.consecutive_payments);
            trace.event(DmlEvent::StreakReset {
                pro_tx,
                from,
            });
            self.update_hot_with(&pro_tx, |hot| hot.consecutive_payments = 0)?;
        }
        Ok(())
    }
}

/// `CSpecialTxProcessor::RebuildListFromBlock(block, pindexPrev, is_v24_active, prevList, view,
/// debugLogs = true)` (EV2 §3.2). `members` is the node's member source; `effects` receives the
/// snapshot rows the member computation stores, in Core's place.
// Core: evo/specialtxman.cpp:309 CSpecialTxProcessor::RebuildListFromBlock
#[allow(clippy::too_many_arguments, reason = "Core's parameters, EV2 §2.11")]
pub fn build_new_list_from_block<C: CoinsView + ?Sized>(
    block: &Block,
    prev: PrevBlock<'_>,
    prev_list: &MnList,
    coins: &C,
    members: &DmlMemberSource<'_>,
    rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
    trace: &mut dyn DmlTrace,
) -> Result<BuiltList, DmlBuildError> {
    build(block, prev, prev_list, coins, members, rules, effects, trace)
}

/// The build over either list (EV2b D24).
#[allow(clippy::too_many_arguments, reason = "Core's parameters, EV2 §2.11")]
pub(crate) fn build<L: BuildList, C: CoinsView + ?Sized>(
    block: &Block,
    prev: PrevBlock<'_>,
    prev_list: &L,
    coins: &C,
    members: &DmlMemberSource<'_>,
    rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
    trace: &mut dyn DmlTrace,
) -> Result<BuiltList<L>, DmlBuildError> {
    // Core asserts this (v24:317); the chainstate never passes anything else.
    if !(prev_list.is_default() || *prev_list.block_hash() == prev.hash) {
        return Err(DmlBuildError::Store(StoreError::inconsistent(DML_PREV_LIST_MISMATCH)));
    }
    let height = prev.height.wrapping_add(1);
    let mut new = prev_list.start_build(height);

    // Step 0: the payee, from the predecessor before any mutation (v24:325).
    let payee = prev_list.payee(rules.payee());

    // Step 1: confirmations over prevList, banned included (v24:330-343).
    let confirmed = Hash256::from(*prev.hash.as_bytes());
    new.confirm(prev_list, prev.height, rules.params.min_confirmations, &confirmed)?;

    // Step 2: DecreaseScores (v24:345).
    new.decrease_scores()?;

    // Step 3: the special transactions, coinbase skipped, in block order (v24:350-729).
    for tx in block.txs().iter().skip(1) {
        if !tx.is_special_tx_version() {
            continue;
        }
        apply_special_tx(tx, prev, &mut new, coins, members, rules, effects, trace)?;
    }

    // Step 4: collateral spends by every transaction (v24:731-757).
    for tx in block.txs().iter().skip(1) {
        for input in tx.inputs() {
            let prevout = &input.prevout;
            let Some(dmn) = new.get_by_collateral(prevout) else {
                continue;
            };
            if dmn.collateral != *prevout {
                continue;
            }
            // The raw `nType`, without an `IsSpecialTxVersion()` guard (EV2 §3.5).
            if dmn.cold.is_shared() && tx.tx_type() != TxType::ProviderDissolve {
                return Err(DmlBuildError::invalid(BAD_SHARED_COLLATERAL_SPEND));
            }
            let (pro_tx, collateral) = (dmn.pro_tx_hash, dmn.collateral);
            new.remove(&pro_tx)?;
            trace.event(DmlEvent::CollateralSpent {
                pro_tx,
                collateral,
                height,
                total: new.counts().total(),
            });
        }
    }

    // Step 5: payee bookkeeping, even if the payee was banned or revoked in this block
    // (v24:759-775).
    if let Some(paid) = payee.as_ref().and_then(|p| new.get(&p.pro_tx_hash)) {
        let pro_tx = paid.pro_tx_hash;
        let mut hot = paid.hot;
        hot.last_paid_height = height;
        if paid.is_evo() && !rules.mn_rr {
            hot.consecutive_payments = hot.consecutive_payments.wrapping_add(1);
            trace.event(DmlEvent::StreakBumped {
                pro_tx,
                to: hot.consecutive_payments,
            });
        }
        new.update_hot(&pro_tx, hot)?;
    }

    // Step 6: reset the streaks of unpaid evonodes over the snapshot `newList2` (v24:777-790).
    new.reset_streaks(payee.as_ref().map(|p| p.pro_tx_hash), rules.mn_rr, trace)?;

    Ok(BuiltList {
        list: new,
        payee,
    })
}

/// One special transaction of step 3, by `nType` (v24:358-728). Any other type does nothing.
#[allow(clippy::too_many_arguments, reason = "the build's state, passed through")]
fn apply_special_tx<L: BuildList, C: CoinsView + ?Sized>(
    tx: &Tx,
    prev: PrevBlock<'_>,
    new: &mut L,
    coins: &C,
    members: &DmlMemberSource<'_>,
    rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let tx_type = tx.tx_type();
    let is_provider = matches!(
        tx_type,
        TxType::ProviderRegister
            | TxType::ProviderUpdateService
            | TxType::ProviderUpdateRegistrar
            | TxType::ProviderUpdateRevoke
            | TxType::ProviderUpdateShare
            | TxType::ProviderUpdateSharedRegistrar
    );
    if !is_provider && tx_type != TxType::QuorumCommitment {
        return Ok(());
    }
    if is_provider && unknown_pro_tx_version(tx).is_some() {
        return Err(out_of_domain_version());
    }
    let decoded = payload(tx);
    match (tx_type, decoded) {
        (TxType::ProviderRegister, Some(SpecialPayload::ProviderRegister(p))) => {
            pro_reg(tx, p, new, coins, rules, trace)
        }
        (TxType::ProviderUpdateService, Some(SpecialPayload::ProviderUpdateService(p))) => {
            pro_up_serv(p, new, rules, trace)
        }
        (TxType::ProviderUpdateRegistrar, Some(SpecialPayload::ProviderUpdateRegistrar(p))) => {
            pro_up_reg(p, new, rules, trace)
        }
        (TxType::ProviderUpdateRevoke, Some(SpecialPayload::ProviderUpdateRevoke(p))) => {
            pro_up_rev(p, new, rules, trace)
        }
        (TxType::ProviderUpdateShare, Some(SpecialPayload::ProviderUpdateShare(p))) => {
            pro_up_share(p, new, trace)
        }
        (
            TxType::ProviderUpdateSharedRegistrar,
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)),
        ) => pro_up_shared_reg(p, new, rules, trace),
        (TxType::QuorumCommitment, Some(SpecialPayload::QuorumCommitment(qc))) => {
            quorum_commitment(qc, prev, new, members, rules, effects, trace)
        }
        (TxType::QuorumCommitment, _) => Err(DmlBuildError::invalid(BAD_QC_PAYLOAD)),
        _ => Err(DmlBuildError::invalid(BAD_PROTX_PAYLOAD)),
    }
}

/// The unique keys of a payload NetInfo's `GetEntries()`, as Core reads the payload (D7);
/// `None` stands for an entry that is neither an address nor a domain (one that fails
/// `NetInfoEntry::IsTriviallyValid`). Shared with EV2b's check-layer scaffolding (D56).
pub fn payload_entries(net_info: &NetInfo) -> Vec<Option<UniqueKey>> {
    net_props(&crate::netinfo::as_core_reads(net_info))
        .iter()
        .map(|prop| match prop {
            NetProp::Addr {
                prop,
                ..
            } => Some(prop.key.clone()),
            NetProp::Invalid => None,
        })
        .collect()
}

/// `HasOperatorKeyUnderAnyScheme(key.Get(), self)`, gated on V24 by the caller.
fn operator_key_held(list: &impl BuildList, key: &OperatorKey, except: &ProTxHash) -> bool {
    list.has_operator_key_under_any_scheme(&key.clone().get(), except)
}

/// ProRegTx (EV2 §3.2.1; v24:358-453).
// Core: evo/specialtxman.cpp:358-453
fn pro_reg<L: BuildList, C: CoinsView + ?Sized>(
    tx: &Tx,
    p: &ProRegTx,
    new: &mut L,
    coins: &C,
    rules: &DmlRules<'_>,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    let tx_hash = tx.txid();
    let txid = pro_tx_of(&tx_hash);
    let collateral = OutPoint {
        hash: if p.collateral_hash.is_null() {
            tx_hash
        } else {
            p.collateral_hash
        },
        index: p.collateral_index,
    };
    let mn_type = RawMnType(p.mn_type.to_base());

    // Core checks external collateral unless `view.GetBestBlock()` is null (v24:378): connect,
    // `TestBlockValidity` and the miner pass the pprev UTXO set (quirk 26).
    if coins.best_block().is_some() && !p.collateral_hash.is_null() {
        let expected = match p.mn_type {
            PayloadMnType::Regular => 1_000 * COIN,
            PayloadMnType::Evo => 4_000 * COIN,
            _ => MAX_MONEY,
        };
        let coin = coins.coin(&collateral).map_err(|e| {
            DmlBuildError::Store(StoreError::read(format!("collateral coin: {e:?}")))
        })?;
        if coin.is_none_or(|c| c.is_spent() || c.value() != expected) {
            return Err(DmlBuildError::invalid(BAD_PROTX_COLLATERAL));
        }
    }

    // Replacement by collateral reuse: the slot owner, without an equality check (v24:389-401).
    if let Some(replaced) = new.get_by_collateral(&collateral).map(|d| d.pro_tx_hash) {
        new.remove(&replaced)?;
        trace.event(DmlEvent::Replaced {
            pro_tx: replaced,
            collateral,
            height,
            total: new.counts().total(),
        });
    }

    for entry in payload_entries(&p.net_info) {
        match entry {
            Some(key) if new.has_unique(&key) => {
                return Err(DmlBuildError::invalid(BAD_PROTX_DUP_NETINFO_ENTRY));
            }
            Some(_) => {}
            None => return Err(DmlBuildError::invalid(BAD_PROTX_NETINFO_ENTRY)),
        }
    }
    let operator = UniqueKey::operator_key(&p.pub_key_operator);
    let owner_dup = if p.shares.is_empty() {
        new.has_unique(&UniqueKey::key_id(&p.key_id_owner))
    } else {
        p.shares.iter().any(|s| new.has_unique(&UniqueKey::key_id(&s.key_id_owner)))
    };
    if owner_dup || new.has_unique(&operator) {
        return Err(DmlBuildError::invalid(BAD_PROTX_DUP_KEY));
    }

    let cold = from_pro_reg(p, height);
    let mut hot = MnHot::default();
    if crate::netinfo::is_empty(&cold.net_info) {
        // Registered banned until a ProUpServTx (v24:433-436).
        hot.ban_if_not_banned(height);
    }
    if rules.v24 && operator_key_held(new, &cold.pub_key_operator, &ProTxHash::from([0; 32])) {
        return Err(DmlBuildError::invalid(BAD_PROTX_DUP_KEY));
    }
    let dmn = Dmn {
        pro_tx_hash: txid,
        internal_id: u64::from(new.total_registered()),
        collateral,
        operator_reward: p.operator_reward,
        mn_type,
        hot,
        cold: Arc::new(cold),
    };
    new.try_add(Arc::new(dmn), true)?;
    trace.event(DmlEvent::Added {
        pro_tx: txid,
        height,
    });
    Ok(())
}

/// ProUpServTx (EV2 §3.2.2; v24:454-521): the type check first (D1), the NetInfo entries held by
/// another masternode, the hash, the type mismatch, `ApplyProUpServTx`, the revival, the V24
/// scheme-migration probe, `UpdateMN`.
// Core: evo/specialtxman.cpp:454-521
fn pro_up_serv<L: BuildList>(
    p: &ProUpServTx,
    new: &mut L,
    rules: &DmlRules<'_>,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    if !matches!(p.mn_type, PayloadMnType::Regular | PayloadMnType::Evo) {
        return Err(DmlBuildError::invalid(BAD_PROTX_TYPE));
    }
    let pro_tx = pro_tx_of(&p.pro_tx_hash);
    for entry in payload_entries(&p.net_info) {
        match entry {
            Some(key) => {
                if new.unique_owner(&key).is_some_and(|owner| owner.pro_tx_hash != pro_tx) {
                    return Err(DmlBuildError::invalid(BAD_PROTX_DUP_NETINFO_ENTRY));
                }
            }
            None => return Err(DmlBuildError::invalid(BAD_PROTX_NETINFO_ENTRY)),
        }
    }
    let dmn = existing(new, &pro_tx)?;
    if RawMnType(p.mn_type.to_base()) != dmn.mn_type {
        return Err(DmlBuildError::invalid(BAD_PROTX_TYPE_MISMATCH));
    }
    let mut cold = (*dmn.cold).clone();
    let mut hot = dmn.hot;
    let current = cold.state_version();
    let target = if rules.v24 {
        current.max(p.version)
    } else {
        current
    };
    // Before V24 Core keeps the state's version and takes the payload's NetInfo, so a payload of
    // the other NetInfo kind leaves a state its own serializer cannot write. Only an ExtAddr
    // payload (or state) gets there, which needs V24 (`bad-protx-version`): D12.
    if !rules.v24 && ext_addr(p.version) != (cold.version >= EXT_ADDR) {
        return Err(DmlBuildError::Exception(DmlException::OutOfDomain(
            "ProUpServTx of the other NetInfo kind before V24",
        )));
    }
    state::apply_pro_up_serv(&mut cold, p, rules.v24, rules.net_info)?;
    if hot.is_banned() {
        // Only revive when all keys are set; a shared masternode's share owner keys always are.
        let operator_set = !is_default_operator_key(&cold.pub_key_operator);
        if operator_set
            && !cold.key_id_voting.is_null()
            && (cold.is_shared() || !cold.key_id_owner.is_null())
        {
            hot.revive(height);
            trace.event(DmlEvent::Revived {
                pro_tx,
                height,
            });
        }
    }
    if rules.v24
        && is_scheme_migration(current, target)
        && operator_key_held(new, &dmn.cold.pub_key_operator, &pro_tx)
    {
        return Err(DmlBuildError::invalid(BAD_PROTX_DUP_KEY));
    }
    new.update(&pro_tx, hot, Arc::new(cold))?;
    trace.event(DmlEvent::Updated {
        pro_tx,
        height,
    });
    Ok(())
}

/// `IsSchemeMigration(old, new)`: raising a state out of the legacy scheme.
// Core: evo/specialtxman.cpp:46 IsSchemeMigration
fn is_scheme_migration(old: u16, new: u16) -> bool {
    old == 1 && new > 1
}

/// ProUpRegTx (EV2 §3.2.3; v24:522-586).
// Core: evo/specialtxman.cpp:522-586
fn pro_up_reg<L: BuildList>(
    p: &ProUpRegTx,
    new: &mut L,
    rules: &DmlRules<'_>,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    let pro_tx = pro_tx_of(&p.pro_tx_hash);
    let dmn = existing(new, &pro_tx)?;
    if dmn.cold.is_shared() {
        return Err(DmlBuildError::invalid(BAD_PROTX_SHARED_MN));
    }
    let mut cold = (*dmn.cold).clone();
    let mut hot = dmn.hot;
    let old_version = cold.state_version();
    // `CBLSLazyWrapper::operator!=` on the state's copy and the payload's (it may decode both).
    let operator_changed = cold.pub_key_operator != p.pub_key_operator;
    let target = if rules.v24 {
        old_version.max(p.version)
    } else if operator_changed {
        p.version
    } else {
        old_version
    };
    if rules.v24
        && (operator_changed || is_scheme_migration(old_version, target))
        && operator_key_held(new, &p.pub_key_operator, &pro_tx)
    {
        return Err(DmlBuildError::invalid(BAD_PROTX_DUP_KEY));
    }
    if operator_changed {
        cold.reset_operator_fields(p.pub_key_operator.scheme());
        hot.ban_if_not_banned(height);
        cold.pub_key_operator = p.pub_key_operator.clone();
    }
    cold.key_id_voting = p.key_id_voting;
    state::set_state_version(&mut cold, target, dmn.mn_type, rules.net_info)?;
    if ext_addr(target) {
        cold.payouts = if ext_addr(p.version) {
            p.payouts.clone()
        } else {
            legacy_payout_as_list(p.script_payout.as_bytes())
        };
        cold.script_payout.clear();
    } else {
        cold.script_payout = p.script_payout.as_bytes().to_vec();
        cold.payouts.clear();
    }
    new.update(&pro_tx, hot, Arc::new(cold))?;
    trace.event(DmlEvent::Updated {
        pro_tx,
        height,
    });
    Ok(())
}

/// ProUpRevTx (EV2 §3.2.4; v24:587-618).
// Core: evo/specialtxman.cpp:587-618
fn pro_up_rev<L: BuildList>(
    p: &ProUpRevTx,
    new: &mut L,
    rules: &DmlRules<'_>,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    let pro_tx = pro_tx_of(&p.pro_tx_hash);
    let dmn = existing(new, &pro_tx)?;
    let mut cold = (*dmn.cold).clone();
    let mut hot = dmn.hot;
    let old_version = cold.state_version();
    let target = if rules.v24 {
        old_version.max(p.version)
    } else {
        1
    };
    cold.reset_operator_fields(scheme_for(1));
    if target >= 2 {
        state::set_state_version(&mut cold, target, dmn.mn_type, rules.net_info)?;
    }
    hot.ban_if_not_banned(height);
    cold.revocation_reason = p.reason.to_base();
    new.update(&pro_tx, hot, Arc::new(cold))?;
    trace.event(DmlEvent::Revoked {
        pro_tx,
        height,
    });
    Ok(())
}

/// ProUpShareTx (EV2 §3.2.5; v24:619-648).
// Core: evo/specialtxman.cpp:619-648
fn pro_up_share<L: BuildList>(
    p: &ProUpShareTx,
    new: &mut L,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    let pro_tx = pro_tx_of(&p.pro_tx_hash);
    let dmn = existing(new, &pro_tx)?;
    let index = usize::from(p.share_index);
    if !dmn.cold.is_shared() || index >= dmn.cold.shares.len() {
        return Err(DmlBuildError::invalid(BAD_PROUPSHARE_INDEX));
    }
    let mut cold = (*dmn.cold).clone();
    cold.shares[index].script_reward = p.script_reward.clone();
    if !share_list_voting_key_safe(&cold.shares, &cold.key_id_voting) {
        return Err(DmlBuildError::invalid(BAD_PROUPSHARE_PAYEE_REUSE));
    }
    new.update(&pro_tx, dmn.hot, Arc::new(cold))?;
    trace.event(DmlEvent::ShareUpdated {
        pro_tx,
        height,
    });
    Ok(())
}

/// ProUpSharedRegTx (EV2 §3.2.5; v24:649-701).
// Core: evo/specialtxman.cpp:649-701
fn pro_up_shared_reg<L: BuildList>(
    p: &ProUpSharedRegTx,
    new: &mut L,
    rules: &DmlRules<'_>,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let height = new.height();
    let pro_tx = pro_tx_of(&p.pro_tx_hash);
    let dmn = existing(new, &pro_tx)?;
    if !dmn.cold.is_shared() {
        return Err(DmlBuildError::invalid(BAD_PROUPSHAREDREG_NOT_SHARED));
    }
    let mut cold = (*dmn.cold).clone();
    let mut hot = dmn.hot;
    let old_version = cold.state_version();
    let operator_changed = cold.pub_key_operator != p.pub_key_operator;
    if operator_changed {
        cold.reset_operator_fields(p.pub_key_operator.scheme());
        hot.ban_if_not_banned(height);
        cold.pub_key_operator = p.pub_key_operator.clone();
    }
    cold.key_id_voting = p.key_id_voting;
    state::set_state_version(&mut cold, old_version, dmn.mn_type, rules.net_info)?;
    if operator_changed {
        // `SetLegacy(false)`: the flag only, the bytes stay (v24:679-681).
        cold.pub_key_operator.set_scheme(BlsSchemeKind::Basic);
    }
    // Not gated on V24, as in Core (`specialtxman.cpp` v24:688): the ProUpSharedRegTx is a V24
    // transaction already.
    if operator_changed && operator_key_held(new, &p.pub_key_operator, &pro_tx) {
        return Err(DmlBuildError::invalid(BAD_PROTX_DUP_KEY));
    }
    if !share_list_voting_key_safe(&cold.shares, &cold.key_id_voting) {
        return Err(DmlBuildError::invalid(BAD_PROUPSHAREDREG_PAYEE_REUSE));
    }
    new.update(&pro_tx, hot, Arc::new(cold))?;
    trace.event(DmlEvent::SharedRegistrarUpdated {
        pro_tx,
        height,
    });
    Ok(())
}

/// A quorum commitment (EV2 §3.3; v24:702-728) and `HandleQuorumCommitment` (v24:283-298): the
/// members that failed the DKG are punished in member-vector order.
// Core: evo/specialtxman.cpp:702-728, :283 HandleQuorumCommitment
fn quorum_commitment<L: BuildList>(
    qc: &FinalCommitment,
    prev: PrevBlock<'_>,
    new: &mut L,
    members: &DmlMemberSource<'_>,
    rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
    trace: &mut dyn DmlTrace,
) -> Result<(), DmlBuildError> {
    let c = &qc.commitment;
    if c.is_null() {
        return Ok(());
    }
    let params = rules
        .params
        .llmq(c.llmq_type)
        .ok_or_else(|| DmlBuildError::invalid(BAD_QC_COMMITMENT_TYPE))?;
    // `int(nHeight) - int(nHeight) % dkgInterval + quorumIndex`: C's truncating `%` (wrapping
    // where Core has UB; a zero interval is out of domain).
    let qc_height = qc.height.to_u32() as i32;
    let interval = params.dkg_interval;
    if interval == 0 {
        return Err(DmlBuildError::Exception(DmlException::OutOfDomain("dkgInterval 0")));
    }
    let quorum_height = qc_height
        .wrapping_sub(qc_height.wrapping_rem(interval))
        .wrapping_add(i32::from(c.quorum_index.unwrap_or(0)));
    let base = u32::try_from(quorum_height).ok().and_then(|h| prev.chain.ancestor(prev.id, h));
    let base = match base {
        Some(base) if prev.chain.header(base).hash.as_bytes() == c.quorum_hash.as_bytes() => base,
        _ => return Err(DmlBuildError::invalid(BAD_QC_QUORUM_HASH)),
    };
    let quorum = members.members(c.llmq_type, base, effects).map_err(|e| {
        match NodeMemberError::from(e) {
            NodeMemberError::Store(e) => DmlBuildError::Store(e),
            NodeMemberError::UnknownLlmqType(t) => {
                DmlBuildError::Exception(DmlException::Members(t))
            }
        }
    })?;
    for (i, member) in quorum.iter().enumerate() {
        let pro_tx = member.pro_tx_hash;
        if new.get(&pro_tx).is_none() {
            continue;
        }
        match c.valid_members.get(i as u64) {
            Some(true) => {}
            Some(false) => {
                let penalty = new.calc_penalty(66);
                new.pose_punish(&pro_tx, penalty, trace)?;
            }
            None => {
                return Err(DmlBuildError::Exception(DmlException::OutOfDomain(
                    "validMembers shorter than the quorum",
                )));
            }
        }
    }
    Ok(())
}
