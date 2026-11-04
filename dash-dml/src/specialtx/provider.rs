//! Core's contextual checks of the registrations and updates (EV1 §3.3 to §3.6):
//! `CheckProRegTx` (type 1), `CheckProUpServTx` (type 2), `CheckProUpRegTx` (type 3) and
//! `CheckProUpRevTx` (type 4), against the list at `pindexPrev` and the coins view.
//!
//! Each step is Core's, in Core's order, with its class and reason.

use core::fmt::Debug;

use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{
    MnType as PayloadMnType, NetInfo, OutPoint, PlatformNodeId, ProRegTx, ProUpRegTx, ProUpRevTx,
    ProUpServTx,
};
use dash_types::Numeric;

use super::covenant::is_shared_collateral_script;
use super::network::{ProviderNetworkFields, check_provider_network_fields};
use super::payload::{PayloadSlot, validated_payload};
use super::reasons::*;
use super::sig::{
    check_hash_sig_bls, check_hash_sig_canonical, check_hash_sig_ecdsa, check_inputs_hash,
    check_string_sig, make_sign_string, payload_sig_hash, shared_reg_consent_hash,
};
use super::trivial::{
    BASIC_BLS, Destination, LEGACY_BLS, OwnerPayouts, collateral_amount, defined_type,
    extract_destination, is_payout_list_key_safe, is_payout_list_trivially_valid,
    is_valid_payout_script,
};
use super::{CheckResult, SpecialTxCtx, SpecialTxFault, bad_special, consensus};
use crate::netinfo::migration_reject;
use crate::types::ext_addr;
use crate::{Dmn, MnList, MnType, ProTxHash, UniqueKey, payload_entries};

/// A verdict with no fault.
fn verdict<E>(result: Result<(), TxValidationError>) -> CheckResult<E> {
    Ok(result)
}

/// Core's `IsVersionChangeValid(state_version, tx_version, is_v24_active)`: no step down to the
/// legacy scheme once V24 is active.
// Core (rc.4): evo/specialtxman.cpp:1192-1206
pub fn version_change_valid(
    state_version: u16,
    tx_version: u16,
    v24: bool,
) -> Result<(), TxValidationError> {
    if v24 && state_version >= BASIC_BLS && tx_version == LEGACY_BLS {
        return consensus(BAD_PROTX_VERSION_DOWNGRADE);
    }
    Ok(())
}

/// Core's `IsSchemeMigration(old_version, new_version)`: raising a state out of the legacy
/// scheme, which re-encodes its operator key.
// Core (rc.4): evo/specialtxman.cpp:46-49
pub fn is_scheme_migration(old_version: i32, new_version: u16) -> bool {
    old_version == i32::from(LEGACY_BLS) && i32::from(new_version) > i32::from(LEGACY_BLS)
}

/// The script of the coin at `outpoint` in the view, unspent; into `buf`.
fn coin_script<'b, E: Debug>(
    ctx: &SpecialTxCtx<'_, E>,
    outpoint: &OutPoint,
    buf: &'b mut Vec<u8>,
) -> Result<Option<(i64, &'b [u8])>, SpecialTxFault<E>> {
    let Some(coin) = ctx.coins.coin(outpoint).map_err(SpecialTxFault::Coins)? else {
        return Ok(None);
    };
    let mut expanded = [0u8; MAX_SPECIAL_SCRIPT_LEN];
    buf.clear();
    buf.extend_from_slice(coin.script_pubkey().expand(&mut expanded));
    Ok(Some((coin.value(), buf.as_slice())))
}

/// The payload's type, which `IsTriviallyValid` (`IsValidMnType`) has already found defined.
fn payload_mn_type(t: PayloadMnType) -> Result<MnType, TxValidationError> {
    defined_type(t).map_or_else(|| consensus(BAD_PROTX_TYPE), Ok)
}

/// The payload's addresses against the list: each is an address or a domain, else
/// `bad-protx-netinfo-entry`, and one that another masternode holds (`held_by_other`) is
/// `bad-protx-dup-netinfo-entry`.
fn check_entries(
    list: &MnList,
    net_info: &NetInfo,
    held_by_other: impl Fn(&Dmn) -> bool,
) -> Result<(), TxValidationError> {
    for entry in payload_entries(net_info) {
        let Some(key) = entry else {
            return bad_special(BAD_PROTX_NETINFO_ENTRY);
        };
        if list.has_unique(&key) && list.unique_owner(&key).is_some_and(|o| held_by_other(o)) {
            return bad_special(BAD_PROTX_DUP_NETINFO_ENTRY);
        }
    }
    Ok(())
}

/// A payload's id as Core's object holds it: the null id when the payload has none.
fn platform_node_id(id: Option<PlatformNodeId>) -> PlatformNodeId {
    id.unwrap_or_else(|| PlatformNodeId::from([0; 20]))
}

/// `check_sigs && !CheckHashSig(ptx, pubKeyOperator.Get())`: the operator's BLS signature of
/// the payload's `SerializeHash`, under the context's verify scheme.
fn operator_sig<E>(
    ctx: &SpecialTxCtx<'_, E>,
    hash: Option<[u8; 32]>,
    sig: &dash_pkc::bls::BlsEagerSig,
    key: &crate::OperatorKey,
) -> CheckResult<E> {
    if !ctx.evo.check_sigs {
        return verdict(Ok(()));
    }
    check_hash_sig_bls(hash, sig, key, ctx.verifier, ctx.verify_scheme())
        .map_err(|_| SpecialTxFault::VerifyUnavailable)
}

/// Core's `CheckProRegTx` (§3.3, `check_sigs` from the context).
// Core (rc.4): evo/specialtxman.cpp:1208-1381
pub fn check_pro_reg_tx<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    let p = match validated_payload::<ProRegTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24) {
        Ok(p) => p,
        Err(e) => return Ok(Err(e)),
    };
    // No longer allow legacy scheme masternode registration.
    if ctx.rules.v24 && u16::from(p.version) < BASIC_BLS {
        return verdict(consensus(BAD_PROTX_VERSION_DISALLOWED));
    }
    let shared = !p.shares.is_empty();
    let owner_payouts =
        OwnerPayouts::new(u16::from(p.version), &p.payouts, p.script_payout.as_bytes());
    let mn_type = match payload_mn_type(p.mn_type) {
        Ok(t) => t,
        Err(e) => return verdict(Err(e)),
    };
    if mn_type == MnType::Evo && !ctx.rules.evo_shares {
        if shared {
            return verdict(bad_special(BAD_PROTX_SHARES_EVO));
        }
        if owner_payouts.len() > 1 {
            return verdict(bad_special(BAD_PROTX_PAYOUTS_EVO));
        }
    }
    let node_id = platform_node_id(p.platform_node_id);
    let fields = ProviderNetworkFields {
        net_info: &p.net_info,
        mn_type,
        version: u16::from(p.version),
        platform_node_id: Some(&node_id),
        platform_p2p_port: p.platform_p2p_port.unwrap_or(0),
        platform_http_port: p.platform_http_port.unwrap_or(0),
    };
    if let Err(e) = check_provider_network_fields(&fields, true, &ctx.net_info) {
        return verdict(Err(e));
    }

    let expected_collateral = collateral_amount(mn_type);
    let mut script = Vec::new();
    // The collateral's destination; `None` for a shared collateral, which has none.
    let mut collateral_dest: Option<Destination> = None;
    // The key that must sign the payload: an external collateral's.
    let mut key_for_payload_sig: Option<[u8; 20]> = None;
    let collateral = if !p.collateral_hash.is_null() {
        let outpoint = OutPoint {
            hash: p.collateral_hash,
            index: p.collateral_index,
        };
        let Some((value, coin_script)) = coin_script(ctx, &outpoint, &mut script)? else {
            return verdict(bad_special(BAD_PROTX_COLLATERAL));
        };
        if value != expected_collateral {
            return verdict(bad_special(BAD_PROTX_COLLATERAL));
        }
        let Some(dest) = extract_destination(coin_script) else {
            return verdict(bad_special(BAD_PROTX_COLLATERAL_DEST));
        };
        // Only a P2PK or P2PKH collateral gives a key that can sign; a P2SH one cannot.
        let Destination::PubKeyHash(key) = dest else {
            return verdict(bad_special(BAD_PROTX_COLLATERAL_PKH));
        };
        collateral_dest = Some(dest);
        key_for_payload_sig = Some(key);
        outpoint
    } else {
        let Some(out) = tx.outputs().get(p.collateral_index as usize) else {
            return verdict(bad_special(BAD_PROTX_COLLATERAL_INDEX));
        };
        if out.value != expected_collateral {
            return verdict(bad_special(BAD_PROTX_COLLATERAL));
        }
        if shared {
            // A shared collateral pays exactly the template script; it has no destination.
            if !is_shared_collateral_script(out.script_pubkey.as_bytes()) {
                return verdict(bad_special(BAD_PROTX_SHARES_COLLATERAL_SCRIPT));
            }
        } else {
            let Some(dest) = extract_destination(out.script_pubkey.as_bytes()) else {
                return verdict(bad_special(BAD_PROTX_COLLATERAL_DEST));
            };
            collateral_dest = Some(dest);
        }
        OutPoint {
            hash: tx.txid(),
            index: p.collateral_index,
        }
    };

    // The collateral key is not to be reused for the other keys (a shared collateral has no
    // destination; its refund and reward keys are the share list's check).
    if !shared && let Some(dest) = &collateral_dest {
        let reuse = ext_addr(u16::from(p.version));
        if let Err(e) =
            is_payout_list_key_safe(owner_payouts, dest, &p.key_id_owner, &p.key_id_voting, reuse)
        {
            return verdict(Err(e));
        }
    }

    let list = ctx.list;
    // An address may be reused only for the same collateral (which replaces the old masternode).
    if let Err(e) = check_entries(list, &p.net_info, |o| o.collateral != collateral) {
        return verdict(Err(e));
    }
    // Never a duplicate key, even for a registration that replaces a masternode. Share owner
    // keys share the owner keys' namespace.
    let operator = UniqueKey::operator_key(&p.pub_key_operator);
    if shared {
        if p.shares.iter().any(|s| list.has_unique(&UniqueKey::key_id(&s.key_id_owner))) {
            return verdict(bad_special(BAD_PROTX_DUP_KEY));
        }
        if list.has_unique(&operator) {
            return verdict(bad_special(BAD_PROTX_DUP_KEY));
        }
    } else if list.has_unique(&UniqueKey::key_id(&p.key_id_owner)) || list.has_unique(&operator) {
        return verdict(bad_special(BAD_PROTX_DUP_KEY));
    }
    // The operator key under the other encoding; nothing is excluded.
    if ctx.rules.v24
        && list.has_operator_key_under_any_scheme(
            &p.pub_key_operator.clone().get(),
            &ProTxHash::from([0; 32]),
        )
    {
        return verdict(bad_special(BAD_PROTX_DUP_KEY));
    }
    if mn_type == MnType::Evo && list.has_unique(&UniqueKey::platform_node_id(&node_id)) {
        return verdict(bad_special(BAD_PROTX_DUP_PLATFORMNODEID));
    }
    if !ctx.dip3_enforced() && p.key_id_owner != p.key_id_voting {
        return verdict(bad_special(BAD_PROTX_KEY_NOT_SAME));
    }

    if let Err(e) = check_inputs_hash(tx, &p.inputs_hash) {
        return verdict(Err(e));
    }
    let check_sigs = ctx.evo.check_sigs;
    if let Some(key) = key_for_payload_sig {
        // The collateral is not part of this transaction: its key proves ownership.
        if check_sigs {
            let message = make_sign_string(p, ctx.evo.cons.params);
            return verdict(check_string_sig(
                message.as_deref(),
                &key.into(),
                p.vch_sig.as_slice(),
            ));
        }
    } else {
        // The collateral is this transaction's own output, so the issuer owns it.
        if !p.vch_sig.as_slice().is_empty() {
            return verdict(consensus(BAD_PROTX_SIG));
        }
        if shared && check_sigs {
            // Every share owner consents to the inputs, outputs, shares, penalty and registrar.
            let consent = shared_reg_consent_hash(p, tx);
            for (share, sig) in p.shares.iter().zip(&p.join_sigs) {
                if let Err(e) = check_hash_sig_canonical(
                    consent,
                    &share.key_id_owner,
                    sig.as_bytes(),
                    TxValidationResult::BadSpecial,
                    BAD_PROTX_SHARES_SIG,
                ) {
                    return verdict(Err(e));
                }
            }
        }
    }
    verdict(Ok(()))
}

/// Core's `CheckProUpRegTx` (§3.5, `check_sigs` from the context).
// Core (rc.4): evo/specialtxman.cpp:1488-1588
pub fn check_pro_up_reg_tx<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    let p = match validated_payload::<ProUpRegTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24) {
        Ok(p) => p,
        Err(e) => return Ok(Err(e)),
    };
    let list = ctx.list;
    let pro_tx = ProTxHash::from(*p.pro_tx_hash.as_bytes());
    let Some(dmn) = list.get(&pro_tx) else {
        return verdict(consensus(BAD_PROTX_HASH));
    };
    let state = &dmn.cold;
    // A shared masternode has no registrar owner key: ProUpShareTx and ProUpSharedRegTx update
    // it.
    if state.is_shared() {
        return verdict(bad_special(BAD_PROTX_SHARED_MN));
    }
    let state_version = state.state_version();
    if let Err(e) = version_change_valid(state_version, u16::from(p.version), ctx.rules.v24) {
        return verdict(Err(e));
    }
    let target_version = state_version.max(u16::from(p.version));
    if ctx.rules.v24 {
        // `CBLSLazyWrapper::operator==`, the payload's key on the left (D17).
        let key_changed = !(p.pub_key_operator == state.pub_key_operator);
        let migrating = is_scheme_migration(state.version, u16::from(p.version));
        if (key_changed || migrating)
            && list.has_operator_key_under_any_scheme(&p.pub_key_operator.clone().get(), &pro_tx)
        {
            return verdict(bad_special(BAD_PROTX_DUP_KEY));
        }
        // A key change clears the addresses, leaving nothing to migrate; otherwise the state's
        // addresses must carry into the target version (`SetStateVersion`, §3.16.1). Only an
        // Evo type changes the step, so an undefined stored type stands as a regular one.
        if !key_changed {
            let mn_type = dmn.mn_type.stored_type().unwrap_or(MnType::Regular);
            if let Some(reason) = migration_reject(
                &state.net_info,
                state_version,
                target_version,
                mn_type,
                &ctx.net_info,
            ) {
                return verdict(bad_special(reason));
            }
        }
    }

    let owner_payouts =
        OwnerPayouts::new(u16::from(p.version), &p.payouts, p.script_payout.as_bytes());
    if let Err(e) =
        is_payout_list_trivially_valid(owner_payouts, &state.key_id_owner, &p.key_id_voting)
    {
        return verdict(Err(e));
    }
    if dmn.is_evo() && !ctx.rules.evo_shares && owner_payouts.len() > 1 {
        return verdict(bad_special(BAD_PROTX_PAYOUTS_EVO));
    }
    let mut script = Vec::new();
    // Never the case: there would be no masternode otherwise.
    let Some((_, coin_script)) = coin_script(ctx, &dmn.collateral, &mut script)? else {
        return verdict(consensus(BAD_PROTX_COLLATERAL));
    };
    let Some(collateral_dest) = extract_destination(coin_script) else {
        return verdict(consensus(BAD_PROTX_COLLATERAL_DEST));
    };
    let reuse = ext_addr(target_version);
    if let Err(e) = is_payout_list_key_safe(
        owner_payouts,
        &collateral_dest,
        &state.key_id_owner,
        &p.key_id_voting,
        reuse,
    ) {
        return verdict(Err(e));
    }
    if list
        .unique_owner(&UniqueKey::operator_key(&p.pub_key_operator))
        .is_some_and(|other| other.pro_tx_hash != pro_tx)
    {
        return verdict(bad_special(BAD_PROTX_DUP_KEY));
    }
    if !ctx.dip3_enforced() && state.key_id_owner != p.key_id_voting {
        return verdict(bad_special(BAD_PROTX_KEY_NOT_SAME));
    }
    if let Err(e) = check_inputs_hash(tx, &p.inputs_hash) {
        return verdict(Err(e));
    }
    if ctx.evo.check_sigs {
        return verdict(check_hash_sig_ecdsa(
            payload_sig_hash(p),
            &state.key_id_owner,
            p.vch_sig.as_slice(),
        ));
    }
    verdict(Ok(()))
}

/// Core's `CheckProUpServTx` (§3.4, `check_sigs` from the context).
// Core (rc.4): evo/specialtxman.cpp:1383-1486
pub fn check_pro_up_serv_tx<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    let p = match validated_payload::<ProUpServTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24)
    {
        Ok(p) => p,
        Err(e) => return Ok(Err(e)),
    };
    let version = u16::from(p.version);
    let mn_type = match payload_mn_type(p.mn_type) {
        Ok(t) => t,
        Err(e) => return verdict(Err(e)),
    };
    let node_id = platform_node_id(p.platform_node_id);
    let fields = ProviderNetworkFields {
        net_info: &p.net_info,
        mn_type,
        version,
        platform_node_id: Some(&node_id),
        platform_p2p_port: p.platform_p2p_port.unwrap_or(0),
        platform_http_port: p.platform_http_port.unwrap_or(0),
    };
    if let Err(e) = check_provider_network_fields(&fields, false, &ctx.net_info) {
        return verdict(Err(e));
    }
    let list = ctx.list;
    let pro_tx = ProTxHash::from(*p.pro_tx_hash.as_bytes());
    let Some(dmn) = list.get(&pro_tx) else {
        return verdict(consensus(BAD_PROTX_HASH));
    };
    if p.mn_type.to_base() != dmn.mn_type.0 {
        return verdict(consensus(BAD_PROTX_TYPE_MISMATCH));
    }
    let state = &dmn.cold;
    let state_version = state.state_version();
    if let Err(e) = version_change_valid(state_version, version, ctx.rules.v24) {
        return verdict(Err(e));
    }
    if ctx.rules.v24 && mn_type == MnType::Evo && !ext_addr(version) {
        return verdict(consensus(BAD_PROTX_VERSION_DISALLOWED));
    }
    if ctx.rules.v24 {
        // `ApplyProUpServTx` on a copy of the state: the payload's version and addresses, raised
        // to the higher of the two versions (§3.16.1).
        let target_version = state_version.max(version);
        if let Some(reason) =
            migration_reject(&p.net_info, version, target_version, mn_type, &ctx.net_info)
        {
            return verdict(bad_special(reason));
        }
    }
    if ctx.rules.v24
        && is_scheme_migration(state.version, version)
        && list.has_operator_key_under_any_scheme(&state.pub_key_operator.clone().get(), &pro_tx)
    {
        return verdict(bad_special(BAD_PROTX_DUP_KEY));
    }
    // An address may be reused only by this masternode.
    if let Err(e) = check_entries(list, &p.net_info, |o| o.pro_tx_hash != pro_tx) {
        return verdict(Err(e));
    }
    let platform = UniqueKey::platform_node_id(&node_id);
    if mn_type == MnType::Evo
        && list.has_unique(&platform)
        && list.unique_owner(&platform).is_some_and(|o| o.pro_tx_hash != pro_tx)
    {
        return verdict(bad_special(BAD_PROTX_DUP_PLATFORMNODEID));
    }
    let operator_payout = p.script_operator_payout.as_bytes();
    if !operator_payout.is_empty()
        && (dmn.operator_reward == 0 || !is_valid_payout_script(operator_payout))
    {
        return verdict(bad_special(BAD_PROTX_OPERATOR_PAYEE));
    }
    if let Err(e) = check_inputs_hash(tx, &p.inputs_hash) {
        return verdict(Err(e));
    }
    operator_sig(ctx, payload_sig_hash(p), &p.sig, &state.pub_key_operator)
}

/// Core's `CheckProUpRevTx` (§3.6, `check_sigs` from the context).
// Core (rc.4): evo/specialtxman.cpp:1590-1621
pub fn check_pro_up_rev_tx<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    let p = match validated_payload::<ProUpRevTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24) {
        Ok(p) => p,
        Err(e) => return Ok(Err(e)),
    };
    let pro_tx = ProTxHash::from(*p.pro_tx_hash.as_bytes());
    let Some(dmn) = ctx.list.get(&pro_tx) else {
        return verdict(consensus(BAD_PROTX_HASH));
    };
    let state = &dmn.cold;
    if let Err(e) = version_change_valid(state.state_version(), u16::from(p.version), ctx.rules.v24)
    {
        return verdict(Err(e));
    }
    if let Err(e) = check_inputs_hash(tx, &p.inputs_hash) {
        return verdict(Err(e));
    }
    operator_sig(ctx, payload_sig_hash(p), &p.sig, &state.pub_key_operator)
}
