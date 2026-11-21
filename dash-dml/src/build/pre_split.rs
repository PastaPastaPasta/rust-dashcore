//! The frozen pre-split oracle: `RebuildListFromBlock` (v24.0.0 `evo/specialtxman.cpp:807-822`)
//! as one function, copied verbatim from the whole-block build before it was split into
//! [`ListBuilder`](super::builder::ListBuilder)'s phases (`da33d228d1`). Tests only; it shares
//! nothing with the builder but `apply_special_tx` and the list's own methods, so a step moved
//! to the wrong phase (a collateral spend into `apply_tx`, the confirmations or the PoSe decay
//! out of `begin`) shows as a difference from it. Do not refactor it onto the builder.

use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::Block;
use dash_evo::effect::EffectSink;
use dash_num::Hash256;
use dash_primitives::TxType;

use super::{
    BAD_SHARED_COLLATERAL_SPEND, BuildList, BuiltList, DML_PREV_LIST_MISMATCH, DmlBuildError,
    DmlRules, PrevBlock, apply_special_tx,
};
use crate::store::{DmlMemberSource, StoreError};
use crate::trace::{DmlEvent, DmlTrace};

/// The whole-block build as it was before the split, steps 0-6 in one body.
#[allow(clippy::too_many_arguments, reason = "Core's parameters, EV2 §2.11")]
pub(crate) fn rebuild_list_from_block<L: BuildList, C: CoinsView + ?Sized>(
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
            let prevout = &input.prevout();
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
