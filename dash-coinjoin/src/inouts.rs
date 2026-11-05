//! Entry and final-transaction validation: Core's `CCoinJoinBaseSession::IsValidInOuts` and the
//! post-V24 promotion, demotion and composition checks (`src/coinjoin/coinjoin.cpp`).

use std::collections::BTreeSet;

use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut};

use crate::denom::{self, PROMOTION_RATIO_COUNT, SessionDenomCounts};
use crate::pool::PoolMessage;

/// An unspent coin as Core's `CCoinsViewMemPool` returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewCoin {
    /// The output.
    pub out: TxOut,
    /// Whether the coin comes from the mempool (`nHeight == MEMPOOL_HEIGHT`).
    pub in_mempool: bool,
}

/// The node's coins as the CoinJoin checks read them: Core's `CCoinsViewMemPool` over the active
/// chain tip and the mempool, and the InstantSend manager's lock set.
pub trait CoinView {
    /// `CCoinsViewMemPool::GetCoin` (`txmempool.cpp:1866`): the output of a mempool
    /// transaction if `outpoint.hash` is one (`None` when the index is past its outputs),
    /// otherwise the chain tip's coin; `None` when missing or spent at the tip (Core's
    /// `!GetCoin(..) || coin.IsSpent()`). A coin some mempool transaction already spends is
    /// still returned: only the tip decides spentness.
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin>;

    /// `CInstantSendManager::IsLocked(txid)`.
    fn is_instantsend_locked(&self, txid: &TxHash) -> bool;
}

/// The coin `prevout` spends, unless it is missing, spent, or an unlocked mempool coin: the
/// common input rule of `IsValidInOuts` and `IsCollateralValid`.
pub(crate) fn usable_coin<V: CoinView + ?Sized>(view: &V, prevout: &OutPoint) -> Option<TxOut> {
    let coin = view.coin(prevout)?;
    if coin.in_mempool && !view.is_instantsend_locked(&prevout.hash) {
        return None;
    }
    Some(coin.out)
}

// Core: script/script.cpp:202 CScript::IsPayToPublicKeyHash
/// `CScript::IsPayToPublicKeyHash`: exactly `OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY
/// OP_CHECKSIG`.
pub fn is_pay_to_public_key_hash(script: &[u8]) -> bool {
    script.len() == 25
        && script[0] == 0x76
        && script[1] == 0xa9
        && script[2] == 0x14
        && script[23] == 0x88
        && script[24] == 0xac
}

/// Why an entry or a final transaction failed: Core's `nMessageIDRet` and, for an entry, whether
/// the server charges the participant's collateral (`fConsumeCollateralRet`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InOutsRejection {
    /// The answer to send.
    pub message: PoolMessage,
    /// Whether the participant's collateral is consumed.
    pub consume_collateral: bool,
}

impl InOutsRejection {
    const fn charged(message: PoolMessage) -> Self {
        Self {
            message,
            consume_collateral: true,
        }
    }

    const fn free(message: PoolMessage) -> Self {
        Self {
            message,
            consume_collateral: false,
        }
    }
}

/// What [`is_valid_in_outs`] checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InOutsKind {
    /// One participant's entry (`fFinalTx == false`).
    Entry,
    /// The session's final transaction (`fFinalTx == true`).
    FinalTx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryType {
    Standard,
    Promotion,
    Demotion,
    FinalTx,
}

// Core: coinjoin/coinjoin.cpp:264 CCoinJoinBaseSession::IsValidInOuts
/// `IsValidInOuts`: whether `inputs` and `outputs` (an entry, or the final transaction) mix at
/// `session_denom`, checked in Core's order. `allow_rebalance_shapes` is Core's
/// `fAllowRebalanceShapes`: promotion and demotion entries, and a final transaction that holds
/// them, are allowed only with it.
///
/// On success, the counts of inputs and outputs at the session denomination (Core's
/// `pDenomCountsRet`).
pub fn is_valid_in_outs<V: CoinView + ?Sized>(
    view: &V,
    inputs: &[TxIn],
    outputs: &[TxOut],
    session_denom: i32,
    allow_rebalance_shapes: bool,
    kind: InOutsKind,
) -> Result<SessionDenomCounts, InOutsRejection> {
    let ratio = PROMOTION_RATIO_COUNT;
    let entry_type = if kind == InOutsKind::FinalTx && allow_rebalance_shapes {
        EntryType::FinalTx
    } else if inputs.len() == outputs.len() {
        EntryType::Standard
    } else if allow_rebalance_shapes && inputs.len() == ratio && outputs.len() == 1 {
        EntryType::Promotion
    } else if allow_rebalance_shapes && inputs.len() == 1 && outputs.len() == ratio {
        EntryType::Demotion
    } else {
        // Pre-V24 a size mismatch; post-V24 an invalid entry structure. Both answer the same.
        return Err(InOutsRejection::charged(PoolMessage::ErrSizeMismatch));
    };

    let mut expected_input_denom = session_denom;
    let mut expected_output_denom = session_denom;
    // Final transaction only: promotion outputs and demotion inputs are at the larger adjacent
    // denomination. 0 never matches.
    let mut larger_denom = 0;
    match entry_type {
        EntryType::Promotion => {
            validate_promotion_entry(inputs, outputs, session_denom)
                .map_err(InOutsRejection::charged)?;
            expected_output_denom = denom::larger_adjacent_denom(session_denom);
        }
        EntryType::Demotion => {
            validate_demotion_entry(inputs, outputs, session_denom)
                .map_err(InOutsRejection::charged)?;
            expected_input_denom = denom::larger_adjacent_denom(session_denom);
        }
        EntryType::FinalTx => larger_denom = denom::larger_adjacent_denom(session_denom),
        EntryType::Standard => {}
    }

    let mut scripts = BTreeSet::new();
    let mut check_txout = |txout: &TxOut, expected_denom: i32| -> Result<(), InOutsRejection> {
        let d = denom::amount_to_denomination(txout.value);
        if d != expected_denom && (larger_denom == 0 || d != larger_denom) {
            return Err(InOutsRejection::charged(PoolMessage::ErrDenom));
        }
        let script = txout.script_pubkey.as_bytes();
        if !is_pay_to_public_key_hash(script) {
            return Err(InOutsRejection::charged(PoolMessage::ErrInvalidScript));
        }
        // No script twice across all inputs and outputs (privacy).
        if !scripts.insert(script.to_vec()) {
            return Err(InOutsRejection::charged(PoolMessage::ErrAlreadyHave));
        }
        Ok(())
    };
    let is_larger =
        |value: i64| larger_denom != 0 && denom::amount_to_denomination(value) == larger_denom;

    // At a valid session denomination every amount below passed the denomination check, so the
    // sums cannot overflow. At denomination 0, which no session has (the server refuses it before
    // a session starts), `amount_to_denomination` passes every non-denominated amount, so the
    // sums wrap as Core's `CAmount` arithmetic does in practice (signed overflow there) rather
    // than panic.
    let mut fees: i64 = 0;
    let mut larger_inputs = 0;
    let mut larger_outputs = 0;
    for txout in outputs {
        check_txout(txout, expected_output_denom)?;
        larger_outputs += usize::from(is_larger(txout.value));
        fees = fees.wrapping_sub(txout.value);
    }

    for txin in inputs {
        if txin.prevout.is_null() {
            return Err(InOutsRejection::charged(PoolMessage::ErrInvalidInput));
        }
        let Some(coin) = usable_coin(view, &txin.prevout) else {
            return Err(InOutsRejection::free(PoolMessage::ErrMissingTx));
        };
        check_txout(&coin, expected_input_denom)?;
        larger_inputs += usize::from(is_larger(coin.value));
        fees = fees.wrapping_add(coin.value);
    }

    // No fees in CoinJoin: standard, promotion and demotion entries all preserve value.
    if fees != 0 {
        return Err(InOutsRejection::free(PoolMessage::ErrFees));
    }

    if entry_type == EntryType::FinalTx
        && !validate_final_tx_composition(
            inputs.len(),
            outputs.len(),
            larger_inputs,
            larger_outputs,
        )
    {
        return Err(InOutsRejection::free(PoolMessage::ErrSizeMismatch));
    }

    Ok(SessionDenomCounts {
        inputs: inputs.len() - larger_inputs,
        outputs: outputs.len() - larger_outputs,
    })
}

// Core: coinjoin/coinjoin.cpp:654 CoinJoin::ValidatePromotionEntry
/// `ValidatePromotionEntry`: [`denom::PROMOTION_RATIO`] inputs at `session_denom` (checked
/// against the coins by [`is_valid_in_outs`]) to one P2PKH output at the next larger
/// denomination.
pub fn validate_promotion_entry(
    inputs: &[TxIn],
    outputs: &[TxOut],
    session_denom: i32,
) -> Result<(), PoolMessage> {
    if inputs.len() != PROMOTION_RATIO_COUNT || outputs.len() != 1 {
        return Err(PoolMessage::ErrSizeMismatch);
    }
    let larger = denom::larger_adjacent_denom(session_denom);
    if larger == 0 || denom::amount_to_denomination(outputs[0].value) != larger {
        return Err(PoolMessage::ErrDenom);
    }
    if !is_pay_to_public_key_hash(outputs[0].script_pubkey.as_bytes()) {
        return Err(PoolMessage::ErrInvalidScript);
    }
    Ok(())
}

// Core: coinjoin/coinjoin.cpp:705 CoinJoin::ValidateDemotionEntry
/// `ValidateDemotionEntry`: one input at the next larger denomination (checked against the coin
/// by [`is_valid_in_outs`]) to [`denom::PROMOTION_RATIO`] P2PKH outputs at `session_denom`.
pub fn validate_demotion_entry(
    inputs: &[TxIn],
    outputs: &[TxOut],
    session_denom: i32,
) -> Result<(), PoolMessage> {
    if inputs.len() != 1 || outputs.len() != PROMOTION_RATIO_COUNT {
        return Err(PoolMessage::ErrSizeMismatch);
    }
    if denom::larger_adjacent_denom(session_denom) == 0 {
        return Err(PoolMessage::ErrDenom);
    }
    for txout in outputs {
        if denom::amount_to_denomination(txout.value) != session_denom {
            return Err(PoolMessage::ErrDenom);
        }
        if !is_pay_to_public_key_hash(txout.script_pubkey.as_bytes()) {
            return Err(PoolMessage::ErrInvalidScript);
        }
    }
    Ok(())
}

// Core: coinjoin/coinjoin.cpp:755 CoinJoin::ValidateFinalTxComposition
/// `ValidateFinalTxComposition`: whether a final transaction with these totals and these counts
/// at the larger denomination is composable from valid entries: each larger output takes
/// [`denom::PROMOTION_RATIO`] session inputs and each larger input gives as many session outputs.
pub const fn validate_final_tx_composition(
    total_inputs: usize,
    total_outputs: usize,
    larger_inputs: usize,
    larger_outputs: usize,
) -> bool {
    if larger_inputs > total_inputs || larger_outputs > total_outputs {
        return false;
    }
    let ratio = PROMOTION_RATIO_COUNT;
    let session_inputs = total_inputs - larger_inputs;
    let session_outputs = total_outputs - larger_outputs;
    session_inputs >= larger_outputs.saturating_mul(ratio)
        && session_outputs >= larger_inputs.saturating_mul(ratio)
}

// Core: primitives/transaction.h:411 CompareInputBIP69
/// BIP69's input order: the previous transaction's hash as displayed, then the index.
pub fn bip69_input_cmp(a: &TxIn, b: &TxIn) -> std::cmp::Ordering {
    if a.prevout.hash == b.prevout.hash {
        return a.prevout.index.cmp(&b.prevout.index);
    }
    a.prevout.hash.as_bytes().iter().rev().cmp(b.prevout.hash.as_bytes().iter().rev())
}

// Core: primitives/transaction.h:428 CompareOutputBIP69
/// BIP69's output order: the value, then the script as Core's `prevector` orders it (shorter
/// first, then bytewise).
pub fn bip69_output_cmp(a: &TxOut, b: &TxOut) -> std::cmp::Ordering {
    let (sa, sb) = (a.script_pubkey.as_bytes(), b.script_pubkey.as_bytes());
    a.value.cmp(&b.value).then(sa.len().cmp(&sb.len())).then_with(|| sa.cmp(sb))
}

/// Sorts a transaction's inputs and outputs in BIP69 order. Core's `std::sort` is unstable;
/// elements that compare equal here are byte-identical, so the order is the same (CJ0 notes D10).
pub fn sort_bip69(tx: &mut Transaction) {
    tx.inputs.sort_by(bip69_input_cmp);
    tx.outputs.sort_by(bip69_output_cmp);
}

/// Whether a transaction is already in BIP69 order, that is, whether [`sort_bip69`] would leave
/// it unchanged (the sort is stable). Core sorts a copy and compares the hashes.
pub fn is_bip69_sorted(tx: &Transaction) -> bool {
    tx.inputs.is_sorted_by(|a, b| bip69_input_cmp(a, b).is_le())
        && tx.outputs.is_sorted_by(|a, b| bip69_output_cmp(a, b).is_le())
}
