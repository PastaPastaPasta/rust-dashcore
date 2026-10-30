//! Core's trivial checks of the ProTx payloads (EV1 §3.3–§3.9, `IsTriviallyValid`,
//! `providertx.cpp:265-586`) and of the payout and share lists (§3.14, `providertx.cpp:20-164`).
//!
//! Each check is Core's, step for step, with its class and reason; the first failure returns. A
//! per-element loop runs every step of one element before the next element starts (D19). Nothing
//! here is base-sdk's context-free `Checkable` (D3).

use dash_consensus::hash::hash160;
use dash_consensus::subsidy::COIN;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{
    COMPACT_SIG_LEN, CollateralShare, MnType as PayloadMnType, NetInfo, PayoutShare, ProDisTx,
    ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
    RevocationReason, SpecialPayload, TxType,
};
use dash_types::Numeric;

use super::network::is_net_info_trivially_valid;
use super::payload::ProTxPayload;
use super::reasons::*;
use crate::netinfo;
use crate::types::{KeyId, ext_addr};
use crate::{MnType, RawMnType};

/// `ProTxVersion::LegacyBLS`.
pub(crate) const LEGACY_BLS: u16 = 1;
/// `ProTxVersion::BasicBLS`.
pub(crate) const BASIC_BLS: u16 = 2;
/// `ProTxVersion::ExtAddr`, `GetMax(true, true)`: the highest version a ProTx payload has.
const MAX_PROTX_VERSION: u16 = 3;
/// `CURRENT_VERSION` of the v24 shared-masternode payloads (types 10 to 12).
// Core: evo/providertx.h:440, :488, :521
const SHARED_PAYLOAD_VERSION: u16 = 1;
/// The most operator reward, in basis points.
const MAX_OPERATOR_REWARD: u16 = 10_000;

/// `MasternodePayoutShare::{MIN_REWARD, MAX_REWARD}`, in basis points of the owner reward.
// Core: evo/providertx.h:34-35
const MIN_PAYOUT_REWARD: u16 = 100;
const MAX_PAYOUT_REWARD: u16 = 10_000;
/// The most payouts a payout list has.
const MAX_PAYOUTS: usize = 8;
/// `CProRegTx::{MIN_SHARES, MAX_SHARES}`: the share count of a shared masternode, which also
/// bounds the signature count of types 10 and 12.
// Core: evo/providertx.h:134-135
const MIN_SHARES: usize = 2;
const MAX_SHARES: usize = 8;
/// `CProRegTx::MAX_EARLY_PERIOD_BLOCKS`.
// Core: evo/providertx.h:139
const MAX_EARLY_PERIOD_BLOCKS: u32 = 420_480;
/// `CCollateralShare::MIN_AMOUNT`.
// Core: evo/providertx.h:58
const MIN_SHARE_AMOUNT: i64 = 100 * COIN;

fn consensus(reason: &'static str) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::Consensus, reason))
}

fn bad_special(reason: &'static str) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::BadSpecial, reason))
}

/// `GetMnType(nType).collat_amount`, the collateral of a defined type.
// Core: evo/dmn_types.h:34, :39
pub fn collateral_amount(mn_type: MnType) -> i64 {
    match mn_type {
        MnType::Regular => 1000 * COIN,
        MnType::Evo => 4000 * COIN,
    }
}

/// A payload's `nType` when it is a defined type (`IsValidMnType`).
pub(crate) fn defined_type(t: PayloadMnType) -> Option<MnType> {
    MnType::from_raw(RawMnType(t.to_base()))
}

/// Core's `CTxDestination`, of the kinds `ExtractDestination` gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// `PKHash`, of a pay-to-pubkey-hash or pay-to-pubkey script.
    PubKeyHash([u8; 20]),
    /// `ScriptHash`, of a pay-to-script-hash script.
    ScriptHash([u8; 20]),
}

impl Destination {
    /// `CTxDestination(PKHash(key_id))`.
    pub fn of_key(key_id: &KeyId) -> Self {
        Destination::PubKeyHash(*key_id.as_bytes())
    }
}

const OP_DUP: u8 = 0x76;
const OP_HASH160: u8 = 0xa9;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_CHECKSIG: u8 = 0xac;

/// `CScript::IsPayToPublicKeyHash`: the hash of `OP_DUP OP_HASH160 <20> OP_EQUALVERIFY
/// OP_CHECKSIG`.
// Core: script/script.cpp:202-210
pub(crate) fn p2pkh_hash(script: &[u8]) -> Option<[u8; 20]> {
    match script {
        [OP_DUP, OP_HASH160, 20, hash @ .., OP_EQUALVERIFY, OP_CHECKSIG] => (*hash).try_into().ok(),
        _ => None,
    }
}

/// `CScript::IsPayToScriptHash`: the hash of `OP_HASH160 <20> OP_EQUAL`.
// Core: script/script.cpp:213-219
pub(crate) fn p2sh_hash(script: &[u8]) -> Option<[u8; 20]> {
    match script {
        [OP_HASH160, 20, hash @ .., OP_EQUAL] => (*hash).try_into().ok(),
        _ => None,
    }
}

/// Core's `IsValidPayoutScript`: pay-to-pubkey-hash or pay-to-script-hash.
// Core: evo/providertx.cpp:20-23
pub fn is_valid_payout_script(script: &[u8]) -> bool {
    p2pkh_hash(script).is_some() || p2sh_hash(script).is_some()
}

/// Core's `ExtractDestination`: the destination of a pay-to-script-hash, pay-to-pubkey or
/// pay-to-pubkey-hash script; `None` for any other script (`Solver`'s multisig, data and
/// nonstandard). A pay-to-pubkey key must have the size its header announces
/// (`CPubKey::ValidSize`), which is all `CPubKey::IsValid` checks.
// Core: script/standard.cpp:161-189; Solver, MatchPayToPubkey (:42-53)
pub fn extract_destination(script: &[u8]) -> Option<Destination> {
    if let Some(hash) = p2sh_hash(script) {
        return Some(Destination::ScriptHash(hash));
    }
    if let [len @ (33 | 65), key @ .., OP_CHECKSIG] = script
        && key.len() == usize::from(*len)
    {
        let valid_size = match key[0] {
            2 | 3 => 33,
            4 | 6 | 7 => 65,
            _ => 0,
        };
        return (valid_size == key.len()).then(|| Destination::PubKeyHash(hash160(key)));
    }
    p2pkh_hash(script).map(Destination::PubKeyHash)
}

/// Core's `GetOwnerPayouts`: a payload's payout list from `ExtAddr` on, else its payout script
/// at the whole owner reward (`LegacyPayoutAsList`). Borrowed, so nothing is copied.
// Core: evo/providertx.h:92-97; evo/providertx_util.cpp:19-22
#[derive(Clone, Copy, Debug)]
pub enum OwnerPayouts<'a> {
    List(&'a [PayoutShare]),
    Legacy(&'a [u8]),
}

impl<'a> OwnerPayouts<'a> {
    /// The owner payouts of a payload of `version`.
    pub fn new(version: u16, payouts: &'a [PayoutShare], script_payout: &'a [u8]) -> Self {
        if ext_addr(version) {
            OwnerPayouts::List(payouts)
        } else {
            OwnerPayouts::Legacy(script_payout)
        }
    }

    /// The number of payouts.
    pub fn len(&self) -> usize {
        match self {
            OwnerPayouts::List(list) => list.len(),
            OwnerPayouts::Legacy(_) => 1,
        }
    }

    /// Whether there are no payouts.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Each payout's script and reward, in list order.
    pub fn iter(&self) -> impl Iterator<Item = (&'a [u8], u16)> + Clone + use<'a> {
        let (list, legacy) = match *self {
            OwnerPayouts::List(list) => (list, None),
            OwnerPayouts::Legacy(script) => (&[][..], Some((script, MAX_PAYOUT_REWARD))),
        };
        list.iter().map(|p| (p.script_payout.as_bytes(), p.reward)).chain(legacy)
    }
}

/// Core's `IsPayoutListTriviallyValid`. `owner` is skipped when null (ProUpReg passes the null
/// key).
// Core: evo/providertx.cpp:25-61
pub fn is_payout_list_trivially_valid(
    payouts: OwnerPayouts<'_>,
    owner: &KeyId,
    voting: &KeyId,
) -> Result<(), TxValidationError> {
    if payouts.is_empty() || payouts.len() > MAX_PAYOUTS {
        return bad_special(BAD_PROTX_PAYOUTS_COUNT);
    }
    let owner = (!owner.is_null()).then(|| Destination::of_key(owner));
    let voting = Destination::of_key(voting);
    let mut total_reward = 0u32;
    for (i, (script, reward)) in payouts.iter().enumerate() {
        if !(MIN_PAYOUT_REWARD..=MAX_PAYOUT_REWARD).contains(&reward) {
            return bad_special(BAD_PROTX_PAYOUT_REWARD);
        }
        total_reward += u32::from(reward);
        if !is_valid_payout_script(script) {
            return bad_special(BAD_PROTX_PAYEE);
        }
        if payouts.iter().take(i).any(|(earlier, _)| earlier == script) {
            return bad_special(BAD_PROTX_PAYEE_DUP);
        }
        let Some(dest) = extract_destination(script) else {
            return bad_special(BAD_PROTX_PAYEE_DEST);
        };
        if Some(dest) == owner || dest == voting {
            return bad_special(BAD_PROTX_PAYEE_REUSE);
        }
    }
    if total_reward != u32::from(MAX_PAYOUT_REWARD) {
        return bad_special(BAD_PROTX_PAYOUT_REWARD_SUM);
    }
    Ok(())
}

/// Core's `IsShareListTriviallyValid`.
// Core: evo/providertx.cpp:63-130
pub fn is_share_list_trivially_valid(
    shares: &[CollateralShare],
    early_period_blocks: u32,
    early_penalty: i64,
    required_collateral: i64,
    voting: &KeyId,
) -> Result<(), TxValidationError> {
    if !(MIN_SHARES..=MAX_SHARES).contains(&shares.len()) {
        return bad_special(BAD_PROTX_SHARES_COUNT);
    }
    if early_period_blocks > MAX_EARLY_PERIOD_BLOCKS {
        return bad_special(BAD_PROTX_SHARES_EARLY_PERIOD);
    }
    let voting = Destination::of_key(voting);
    // Each amount is at most `required_collateral` and there are at most 8, so the sum fits.
    let mut total_amount = 0i64;
    let mut min_amount = i64::MAX;
    for (i, share) in shares.iter().enumerate() {
        if share.amount < MIN_SHARE_AMOUNT || share.amount > required_collateral {
            return bad_special(BAD_PROTX_SHARES_AMOUNT);
        }
        total_amount += share.amount;
        min_amount = min_amount.min(share.amount);
        if share.key_id_owner.is_null() {
            return bad_special(BAD_PROTX_SHARES_KEY_NULL);
        }
        let earlier = &shares[..i];
        if earlier.iter().any(|e| e.key_id_owner == share.key_id_owner) {
            return bad_special(BAD_PROTX_SHARES_DUP_KEY);
        }
        if earlier.iter().any(|e| e.script_refund == share.script_refund) {
            return bad_special(BAD_PROTX_SHARES_DUP_REFUND);
        }
        // An empty reward script means "use the refund script".
        let reward = (!share.script_reward.is_empty()).then_some(&share.script_reward);
        for script in [Some(&share.script_refund), reward].into_iter().flatten() {
            let script = script.as_bytes();
            if !is_valid_payout_script(script) {
                return bad_special(BAD_PROTX_SHARES_PAYEE);
            }
            let Some(dest) = extract_destination(script) else {
                return bad_special(BAD_PROTX_SHARES_PAYEE_DEST);
            };
            if dest == voting {
                return bad_special(BAD_PROTX_SHARES_PAYEE_REUSE);
            }
            if shares.iter().any(|other| dest == Destination::of_key(&other.key_id_owner)) {
                return bad_special(BAD_PROTX_SHARES_PAYEE_REUSE);
            }
        }
    }
    if total_amount != required_collateral {
        return bad_special(BAD_PROTX_SHARES_AMOUNT_SUM);
    }
    if early_penalty < 0 || early_penalty >= min_amount {
        return bad_special(BAD_PROTX_SHARES_PENALTY);
    }
    // Without an early period the penalty is never required, but it would still be the
    // unilateral bonus ceiling.
    if early_period_blocks == 0 && early_penalty != 0 {
        return bad_special(BAD_PROTX_SHARES_PENALTY);
    }
    Ok(())
}

/// `CCollateralShare::RewardScript()`: the reward script, or the refund script when it is empty.
// Core: evo/providertx.h:80
pub fn reward_script(share: &CollateralShare) -> &[u8] {
    if share.script_reward.is_empty() {
        share.script_refund.as_bytes()
    } else {
        share.script_reward.as_bytes()
    }
}

/// Core's `IsShareListVotingKeySafe`: no share's refund or reward script pays `voting`.
// Core: evo/providertx.cpp:132-144
pub fn is_share_list_voting_key_safe(shares: &[CollateralShare], voting: &KeyId) -> bool {
    let voting = Some(Destination::of_key(voting));
    shares.iter().all(|share| {
        [share.script_refund.as_bytes(), reward_script(share)]
            .into_iter()
            .all(|script| extract_destination(script) != voting)
    })
}

/// Core's `IsPayoutListKeySafe`: neither the owner nor the voting key is the collateral's
/// destination and, when `check_payout_collateral_reuse`, no payout pays it.
// Core: evo/providertx.cpp:146-164
pub fn is_payout_list_key_safe(
    payouts: OwnerPayouts<'_>,
    collateral_dest: &Destination,
    owner: &KeyId,
    voting: &KeyId,
    check_payout_collateral_reuse: bool,
) -> Result<(), TxValidationError> {
    if *collateral_dest == Destination::of_key(owner)
        || *collateral_dest == Destination::of_key(voting)
    {
        return bad_special(BAD_PROTX_COLLATERAL_REUSE);
    }
    if check_payout_collateral_reuse
        && payouts.iter().any(|(script, _)| extract_destination(script) == Some(*collateral_dest))
    {
        return bad_special(BAD_PROTX_PAYEE_REUSE);
    }
    Ok(())
}

/// `pubKeyOperator.IsLegacy()`: the scheme the key was read under.
fn is_legacy(scheme: BlsSchemeKind) -> bool {
    scheme == BlsSchemeKind::Legacy
}

/// The ProTx version and type checks every ProTx with a type field opens with: version 0 or
/// above `ExtAddr`, a legacy-scheme Evo, an undefined type.
fn version_and_type(version: u16, mn_type: PayloadMnType) -> Result<MnType, TxValidationError> {
    if version == 0 || version > MAX_PROTX_VERSION {
        consensus(BAD_PROTX_VERSION)?;
    }
    if version < BASIC_BLS && mn_type == PayloadMnType::Evo {
        consensus(BAD_PROTX_EVO_VERSION)?;
    }
    defined_type(mn_type)
        .ok_or_else(|| TxValidationError::new(TxValidationResult::Consensus, BAD_PROTX_TYPE))
}

/// `netInfo->CanStorePlatform() != (nVersion >= ExtAddr)`: the extended form is the one from
/// version 3.
fn net_info_version(net_info: &NetInfo, version: u16) -> Result<(), TxValidationError> {
    if matches!(net_info, NetInfo::Extended(_)) != ext_addr(version) {
        return consensus(BAD_PROTX_NETINFO_VERSION);
    }
    Ok(())
}

/// Core's `CProRegTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:265-328
pub fn pro_reg_trivially_valid(p: &ProRegTx) -> Result<(), TxValidationError> {
    let mn_type = version_and_type(u16::from(p.version), p.mn_type)?;
    if p.mode != 0 {
        return consensus(BAD_PROTX_MODE);
    }
    let shared = !p.shares.is_empty();
    if shared {
        // The collateral must be internal.
        if !p.collateral_hash.is_null() {
            return bad_special(BAD_PROTX_SHARES_EXTERNAL);
        }
        // The share owner keys replace the owner key.
        if !p.key_id_owner.is_null() {
            return bad_special(BAD_PROTX_SHARES_OWNER_KEY);
        }
        if !p.payouts.is_empty() {
            return bad_special(BAD_PROTX_SHARES_PAYOUTS);
        }
    } else {
        if p.early_period_blocks != 0 || p.early_penalty != 0 {
            return bad_special(BAD_PROTX_SHARES_EMPTY_FIELDS);
        }
        if p.key_id_owner.is_null() {
            return bad_special(BAD_PROTX_KEY_NULL);
        }
    }
    if !p.pub_key_operator.get().is_valid() || p.key_id_voting.is_null() {
        return bad_special(BAD_PROTX_KEY_NULL);
    }
    if is_legacy(p.pub_key_operator.scheme()) != (u16::from(p.version) == LEGACY_BLS) {
        return bad_special(BAD_PROTX_OPERATOR_PUBKEY);
    }
    if shared {
        is_share_list_trivially_valid(
            &p.shares,
            p.early_period_blocks,
            p.early_penalty,
            collateral_amount(mn_type),
            &p.key_id_voting,
        )?;
    } else {
        let payouts =
            OwnerPayouts::new(u16::from(p.version), &p.payouts, p.script_payout.as_bytes());
        is_payout_list_trivially_valid(payouts, &p.key_id_owner, &p.key_id_voting)?;
    }
    net_info_version(&p.net_info, u16::from(p.version))?;
    if !netinfo::is_empty(&p.net_info) {
        is_net_info_trivially_valid(&p.net_info, mn_type)?;
    }
    if p.operator_reward > MAX_OPERATOR_REWARD {
        return bad_special(BAD_PROTX_OPERATOR_REWARD);
    }
    Ok(())
}

/// Core's `CProUpServTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:407-430
pub fn pro_up_serv_trivially_valid(p: &ProUpServTx) -> Result<(), TxValidationError> {
    let mn_type = version_and_type(u16::from(p.version), p.mn_type)?;
    net_info_version(&p.net_info, u16::from(p.version))?;
    if netinfo::is_empty(&p.net_info) {
        return bad_special(BAD_PROTX_NETINFO_EMPTY);
    }
    is_net_info_trivially_valid(&p.net_info, mn_type)
}

/// Core's `CProUpRegTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:449-466
pub fn pro_up_reg_trivially_valid(p: &ProUpRegTx) -> Result<(), TxValidationError> {
    if u16::from(p.version) == 0 || u16::from(p.version) > MAX_PROTX_VERSION {
        return consensus(BAD_PROTX_VERSION);
    }
    if p.mode != 0 {
        return consensus(BAD_PROTX_MODE);
    }
    if !p.pub_key_operator.get().is_valid() || p.key_id_voting.is_null() {
        return bad_special(BAD_PROTX_KEY_NULL);
    }
    if is_legacy(p.pub_key_operator.scheme()) != (u16::from(p.version) == LEGACY_BLS) {
        return bad_special(BAD_PROTX_OPERATOR_PUBKEY);
    }
    let payouts = OwnerPayouts::new(u16::from(p.version), &p.payouts, p.script_payout.as_bytes());
    is_payout_list_trivially_valid(payouts, &KeyId::default(), &p.key_id_voting)
}

/// Core's `CProUpRevTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:476-488
pub fn pro_up_rev_trivially_valid(p: &ProUpRevTx) -> Result<(), TxValidationError> {
    if u16::from(p.version) == 0 || u16::from(p.version) > MAX_PROTX_VERSION {
        return consensus(BAD_PROTX_VERSION);
    }
    // `nReason > REASON_LAST`: base-sdk keeps such a reason as `Unknown`.
    if matches!(p.reason, RevocationReason::Unknown(_)) {
        return consensus(BAD_PROTX_REASON);
    }
    Ok(())
}

/// Core's `CProDisTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:525-534
pub fn pro_dis_trivially_valid(p: &ProDisTx) -> Result<(), TxValidationError> {
    if p.version == 0 || p.version > SHARED_PAYLOAD_VERSION {
        return consensus(BAD_PRODIS_VERSION);
    }
    if p.sigs.is_empty() || p.sigs.len() > MAX_SHARES {
        return bad_special(BAD_PRODIS_SIG_COUNT);
    }
    Ok(())
}

/// Core's `CProUpShareTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:542-554
pub fn pro_up_share_trivially_valid(p: &ProUpShareTx) -> Result<(), TxValidationError> {
    if p.version == 0 || p.version > SHARED_PAYLOAD_VERSION {
        return consensus(BAD_PROUPSHARE_VERSION);
    }
    if p.vch_sig.len() != COMPACT_SIG_LEN {
        return bad_special(BAD_PROUPSHARE_SIG_SIZE);
    }
    if !is_valid_payout_script(p.script_reward.as_bytes()) {
        return bad_special(BAD_PROUPSHARE_PAYEE);
    }
    Ok(())
}

/// Core's `CProUpSharedRegTx::IsTriviallyValid`.
// Core: evo/providertx.cpp:564-579
pub fn pro_up_shared_reg_trivially_valid(p: &ProUpSharedRegTx) -> Result<(), TxValidationError> {
    if p.version == 0 || p.version > SHARED_PAYLOAD_VERSION {
        return consensus(BAD_PROUPSHAREDREG_VERSION);
    }
    if !p.pub_key_operator.get().is_valid() || p.key_id_voting.is_null() {
        return bad_special(BAD_PROTX_KEY_NULL);
    }
    if is_legacy(p.pub_key_operator.scheme()) {
        return bad_special(BAD_PROTX_OPERATOR_PUBKEY);
    }
    if p.sigs.is_empty() || p.sigs.len() > MAX_SHARES {
        return bad_special(BAD_PROUPSHAREDREG_SIG_COUNT);
    }
    Ok(())
}

macro_rules! pro_tx_payload {
    ($t:ty, $variant:ident, $check:ident) => {
        impl ProTxPayload for $t {
            const TX_TYPE: TxType = TxType::$variant;

            fn of(payload: &SpecialPayload) -> Option<&Self> {
                match payload {
                    SpecialPayload::$variant(p) => Some(p),
                    _ => None,
                }
            }

            fn version(&self) -> u16 {
                u16::from(self.version)
            }

            fn is_trivially_valid(&self) -> Result<(), TxValidationError> {
                $check(self)
            }
        }
    };
}

pro_tx_payload!(ProRegTx, ProviderRegister, pro_reg_trivially_valid);
pro_tx_payload!(ProUpServTx, ProviderUpdateService, pro_up_serv_trivially_valid);
pro_tx_payload!(ProUpRegTx, ProviderUpdateRegistrar, pro_up_reg_trivially_valid);
pro_tx_payload!(ProUpRevTx, ProviderUpdateRevoke, pro_up_rev_trivially_valid);
pro_tx_payload!(ProDisTx, ProviderDissolve, pro_dis_trivially_valid);
pro_tx_payload!(ProUpShareTx, ProviderUpdateShare, pro_up_share_trivially_valid);
pro_tx_payload!(ProUpSharedRegTx, ProviderUpdateSharedRegistrar, pro_up_shared_reg_trivially_valid);

#[cfg(test)]
pub(crate) mod tests;
