//! Core's contextual checks of the shared-masternode transactions (EV1 §3.7–§3.9): the
//! dissolution (type 10, `CheckProDisTx`, `CheckProDisTxForList`), the reward update (type 11,
//! `CheckProUpShareTx`) and the shared registrar update (type 12, `CheckProUpSharedRegTx`). All
//! three exist from V24 on.

use dash_consensus::primitives::Tx;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{ProDisTx, ProUpShareTx, ProUpSharedRegTx};

use super::payload::{PayloadSlot, validated_payload};
use super::reasons::*;
use super::sig::{
    check_hash_sig_canonical, check_inputs_hash, payload_sig_hash, pro_dis_sign_hash,
};
use super::trivial::{Destination, extract_destination, is_share_list_voting_key_safe};
use super::{SpecialTxCtx, bad_special, consensus, core_throw};
use crate::{MnList, ProTxHash};

/// `CProDisTx::MAX_FEE`: the most a dissolution leaves to fees, in duffs.
// Core: evo/providertx.h:443
pub const PRODIS_MAX_FEE: i64 = 1_000_000;

/// Core's `ProRataFloor(total, weight, weight_total)`: `floor(total * weight / weight_total)` over
/// the three amounts read as `uint64_t`, its low 64 bits as a `CAmount`. The product of two
/// `u64` is exact in a `u128`, as in Core's `arith_uint256`. `None` for a zero `weight_total`,
/// where Core's division throws.
// Core (rc.4): evo/providertx_util.cpp:88-95
pub fn pro_rata_floor(total: i64, weight: i64, weight_total: i64) -> Option<i64> {
    let product = u128::from(total as u64) * u128::from(weight as u64);
    let quotient = product.checked_div(u128::from(weight_total as u64))?;
    Some(quotient as u64 as i64)
}

/// Core's `CheckProDisTx` (§3.7).
// Core (rc.4): evo/specialtxman.cpp:1849-1873
pub fn check_pro_dis_tx<E>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> Result<(), TxValidationError> {
    let p = validated_payload::<ProDisTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24)?;
    if !ctx.rules.v24 {
        return consensus(BAD_PRODIS_TOO_EARLY);
    }
    // The masternode must exist at the previous block: a same-block registration and
    // dissolution is invalid. What a dissolution is checked against does not change after the
    // registration, so this check is complete.
    if ctx.list.get(&ProTxHash::from(*p.pro_tx_hash.as_bytes())).is_none() {
        return bad_special(BAD_PRODIS_HASH);
    }
    check_pro_dis_tx_for_list(tx, p, ctx.list, ctx.prev_height() + 1, ctx.evo.check_sigs)
}

/// Core's `CheckProDisTxForList` (§3.7): the dissolution against `list` at `spend_height`.
///
/// A zero divisor in the pro-rata floor is Core's throw, here [`core_throw`]'s
/// `failed-check-special-tx`: that is Core's verdict only under `CheckSpecialTxInner`'s catch,
/// its one caller at rc.4 (`specialtxman.cpp:1872`); another caller binds its own.
///
/// The sums wrap where Core's signed arithmetic would overflow, which the amounts a list holds
/// (bounded at registration) and the transaction's (`MoneyRange`, checked before) never reach.
// Core (rc.4): evo/specialtxman.cpp:1736-1847
pub fn check_pro_dis_tx_for_list(
    tx: &Tx,
    p: &ProDisTx,
    list: &MnList,
    spend_height: i32,
    check_sigs: bool,
) -> Result<(), TxValidationError> {
    let Some(dmn) = list.get(&ProTxHash::from(*p.pro_tx_hash.as_bytes())) else {
        return bad_special(BAD_PRODIS_HASH);
    };
    let state = &dmn.cold;
    if !state.is_shared() {
        return bad_special(BAD_PRODIS_NOT_SHARED);
    }
    let shares = &state.shares;

    // Exactly one input: the collateral, with an empty scriptSig (which keeps the txid
    // non-malleable).
    match tx.inputs() {
        [input] if input.prevout == dmn.collateral && input.script_sig.as_bytes().is_empty() => {}
        _ => return bad_special(BAD_PRODIS_INPUT),
    }
    let actor = usize::from(p.actor_index);
    if actor >= shares.len() {
        return bad_special(BAD_PRODIS_ACTOR);
    }

    // One signature (unilateral, the actor's) or one per share (unanimous); two shares at least,
    // so the two never coincide.
    if p.sigs.len() != 1 && p.sigs.len() != shares.len() {
        return bad_special(BAD_PRODIS_SIG_COUNT);
    }
    let unanimous = p.sigs.len() == shares.len();

    let early = i64::from(spend_height) - i64::from(state.registered_height)
        < i64::from(state.early_period_blocks);
    let required_penalty = if unanimous || !early {
        0
    } else {
        state.early_penalty
    };

    // One output per non-actor share paying its refund script, in share order, then at most one
    // actor output.
    let outputs = tx.outputs();
    let non_actor_count = shares.len() - 1;
    if outputs.len() != non_actor_count && outputs.len() != non_actor_count + 1 {
        return bad_special(BAD_PRODIS_PAYEE_COUNT);
    }

    let non_actor = || shares.iter().enumerate().filter(move |&(i, _)| i != actor).map(|(_, s)| s);
    let non_actor_total = non_actor().fold(0i64, |sum, share| sum.wrapping_add(share.amount));

    let mut bonus_total = 0i64;
    for (share, out) in non_actor().zip(outputs) {
        if out.script_pubkey.as_bytes() != share.script_refund.as_bytes() {
            return bad_special(BAD_PRODIS_PAYEE);
        }
        // bonus >= floor(P * amount / W): monotone in P, which keeps a valid dissolution valid.
        let bonus = out.value.wrapping_sub(share.amount);
        let Some(floor) = pro_rata_floor(required_penalty, share.amount, non_actor_total) else {
            return Err(core_throw());
        };
        if bonus < floor {
            return bad_special(BAD_PRODIS_PENALTY_FLOOR);
        }
        bonus_total = bonus_total.wrapping_add(bonus);
    }
    if let Some(actor_out) = outputs.get(non_actor_count) {
        if actor_out.script_pubkey.as_bytes() != shares[actor].script_refund.as_bytes() {
            return bad_special(BAD_PRODIS_PAYEE);
        }
        if actor_out.value == 0 {
            return bad_special(BAD_PRODIS_ACTOR_OUTPUT_ZERO);
        }
    }
    // The floors drop sub-duff remainders; the sum rule puts them on a non-actor output.
    if bonus_total < required_penalty {
        return bad_special(BAD_PRODIS_PENALTY_SUM);
    }
    let out_total = outputs.iter().fold(0i64, |sum, out| sum.wrapping_add(out.value));
    if non_actor_total.wrapping_add(shares[actor].amount).wrapping_sub(out_total) > PRODIS_MAX_FEE {
        return bad_special(BAD_PRODIS_FEE);
    }
    if !unanimous && bonus_total > state.early_penalty {
        return bad_special(BAD_PRODIS_BONUS);
    }

    if check_sigs {
        let sign_hash = Some(pro_dis_sign_hash(p, tx, p.sigs.len() as u8));
        let signers = if unanimous {
            &shares[..]
        } else {
            std::slice::from_ref(&shares[actor])
        };
        for (share, sig) in signers.iter().zip(&p.sigs) {
            check_hash_sig_canonical(
                sign_hash,
                &share.key_id_owner,
                sig.as_bytes(),
                TxValidationResult::BadSpecial,
                BAD_PRODIS_SIG,
            )?;
        }
    }
    Ok(())
}

/// Core's `CheckProUpShareTx` (§3.8).
// Core (rc.4): evo/specialtxman.cpp:1623-1677
pub fn check_pro_up_share_tx<E>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> Result<(), TxValidationError> {
    let p = validated_payload::<ProUpShareTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24)?;
    if !ctx.rules.v24 {
        return consensus(BAD_PROUPSHARE_TOO_EARLY);
    }
    let Some(dmn) = ctx.list.get(&ProTxHash::from(*p.pro_tx_hash.as_bytes())) else {
        return bad_special(BAD_PROUPSHARE_HASH);
    };
    let state = &dmn.cold;
    if !state.is_shared() {
        return bad_special(BAD_PROUPSHARE_NOT_SHARED);
    }
    let Some(share) = state.shares.get(usize::from(p.share_index)) else {
        return bad_special(BAD_PROUPSHARE_INDEX);
    };

    // The new reward script has the registration's payee-reuse rules.
    let Some(dest) = extract_destination(p.script_reward.as_bytes()) else {
        return bad_special(BAD_PROUPSHARE_PAYEE_DEST);
    };
    if dest == Destination::of_key(&state.key_id_voting)
        || state.shares.iter().any(|s| dest == Destination::of_key(&s.key_id_owner))
    {
        return bad_special(BAD_PROUPSHARE_PAYEE_REUSE);
    }

    check_inputs_hash(tx, &p.inputs_hash)?;
    if ctx.evo.check_sigs {
        check_hash_sig_canonical(
            payload_sig_hash(p),
            &share.key_id_owner,
            &p.vch_sig,
            TxValidationResult::BadSpecial,
            BAD_PROUPSHARE_SIG,
        )?;
    }
    Ok(())
}

/// Core's `CheckProUpSharedRegTx` (§3.9).
// Core (rc.4): evo/specialtxman.cpp:1679-1734
pub fn check_pro_up_shared_reg_tx<E>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &SpecialTxCtx<'_, E>,
) -> Result<(), TxValidationError> {
    let p = validated_payload::<ProUpSharedRegTx>(tx.tx_type(), payload, ctx.v19(), ctx.rules.v24)?;
    if !ctx.rules.v24 {
        return consensus(BAD_PROUPSHAREDREG_TOO_EARLY);
    }
    let pro_tx = ProTxHash::from(*p.pro_tx_hash.as_bytes());
    let Some(dmn) = ctx.list.get(&pro_tx) else {
        return bad_special(BAD_PROUPSHAREDREG_HASH);
    };
    let state = &dmn.cold;
    if !state.is_shared() {
        return bad_special(BAD_PROUPSHAREDREG_NOT_SHARED);
    }
    // Unanimity: one signature per share, in share order.
    if p.sigs.len() != state.shares.len() {
        return bad_special(BAD_PROUPSHAREDREG_SIG_COUNT);
    }
    // The operator key under either encoding; ungated, unlike the registrar path's.
    if ctx.list.has_operator_key_under_any_scheme(&p.pub_key_operator.clone().get(), &pro_tx) {
        return bad_special(BAD_PROTX_DUP_KEY);
    }
    if !is_share_list_voting_key_safe(&state.shares, &p.key_id_voting) {
        return bad_special(BAD_PROUPSHAREDREG_PAYEE_REUSE);
    }

    check_inputs_hash(tx, &p.inputs_hash)?;
    if ctx.evo.check_sigs {
        let payload_hash = payload_sig_hash(p);
        for (share, sig) in state.shares.iter().zip(&p.sigs) {
            check_hash_sig_canonical(
                payload_hash,
                &share.key_id_owner,
                sig.as_bytes(),
                TxValidationResult::BadSpecial,
                BAD_PROUPSHAREDREG_SIG,
            )?;
        }
    }
    Ok(())
}
