// SPDX-License-Identifier: MIT

//! The Dash element set of the basic filter: what Core v24's `BasicFilterElements`
//! (`src/blockfilter.cpp`) puts into a block's filter.
//!
//! For every transaction of the block:
//! * each output script, except an empty one and one that starts with `OP_RETURN`;
//! * the elements `ExtractSpecialTxFilterElements` (`src/evo/specialtx_filter.cpp`) names for a
//!   special transaction, the fields an SPV client watches for (collateral outpoints, owner and
//!   voting key ids, payout scripts, ProTx hashes, asset-lock credit outputs).
//!
//! And for every input of a non-coinbase transaction, the script of the output it spends (Core
//! reads them from the block's undo data), except an empty one.
//!
//! The filter counts distinct elements, so the order here does not matter.

use dash_primitives::{OutPoint, ProTxVersion, SpecialPayload, Transaction};
use dash_types::Numeric as _;

use crate::gcs::FilterQuery;

const OP_RETURN: u8 = 0x6a;

/// Adds one script of a transaction output: nothing for an empty script or an `OP_RETURN` one.
fn add_output_script(set: &mut FilterQuery, script: &[u8]) {
    if script.first() != Some(&OP_RETURN) {
        add_script(set, script);
    }
}

/// Core's `AddScriptElement`: a script, unless it is empty. This is also how the script of an
/// output the block spends is added.
pub fn add_script(set: &mut FilterQuery, script: &[u8]) {
    if !script.is_empty() {
        set.push(script);
    }
}

/// Core's `GetOwnerPayouts`: the payout list from ExtAddr on, else the one legacy script.
fn add_owner_payouts(
    set: &mut FilterQuery,
    version: ProTxVersion,
    list: &[dash_primitives::PayoutShare],
    legacy: &[u8],
) {
    if version >= ProTxVersion::ExtAddr {
        for payout in list {
            add_script(set, payout.script_payout.as_bytes());
        }
    } else {
        add_script(set, legacy);
    }
}

/// Adds the output scripts of `tx`.
pub fn add_output_scripts(set: &mut FilterQuery, tx: &Transaction) {
    for output in &tx.outputs {
        add_output_script(set, output.script_pubkey.as_bytes());
    }
}

/// Core's `ExtractSpecialTxFilterElements`: adds the fields of `tx`'s special payload that the
/// filter carries. Nothing for a transaction without a payload, for a payload that does not
/// decode to its end (Core's `GetTxPayload` returns `nullopt`), and for the types that add no
/// field (coinbase, quorum commitment, MNHF signal, asset unlock).
///
/// A payload of version 0 or above the highest known adds nothing here, though Core's serializer
/// leaves the fields at their defaults for one of two bytes and the extraction would add those
/// defaults. It cannot be in a connected block: the `bad-protx-version` checks of
/// `CheckSpecialTx` reject it.
///
/// `tx` must be as decoded from the wire: a [`TxType::Unknown`](dash_primitives::TxType) with a
/// code that names a type would not take that type's payload path.
pub fn add_special_tx_elements(set: &mut FilterQuery, tx: &Transaction) {
    let Some(Ok(decoded)) = tx.decode_payload() else {
        return;
    };
    if !decoded.is_complete() {
        return;
    }
    match &decoded.payload {
        SpecialPayload::ProviderRegister(p) => {
            // The collateral outpoint as `COutPoint` serializes: the hash, then the index.
            let mut outpoint = [0u8; 36];
            outpoint[..32].copy_from_slice(&p.collateral_hash.to_lendian());
            outpoint[32..].copy_from_slice(&p.collateral_index.to_le_bytes());
            set.push(&outpoint);
            set.push(p.key_id_owner.as_bytes());
            set.push(p.key_id_voting.as_bytes());
            add_owner_payouts(set, p.version, &p.payouts, p.script_payout.as_bytes());
            for share in &p.shares {
                add_script(set, share.script_refund.as_bytes());
                // `RewardScript()`: the reward script, or the refund script when it is empty.
                let reward = if share.script_reward.is_empty() {
                    &share.script_refund
                } else {
                    &share.script_reward
                };
                add_script(set, reward.as_bytes());
                set.push(share.key_id_owner.as_bytes());
            }
        }
        SpecialPayload::ProviderUpdateService(p) => {
            set.push(&p.pro_tx_hash.to_lendian());
            add_script(set, p.script_operator_payout.as_bytes());
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            set.push(&p.pro_tx_hash.to_lendian());
            set.push(p.key_id_voting.as_bytes());
            add_owner_payouts(set, p.version, &p.payouts, p.script_payout.as_bytes());
        }
        SpecialPayload::ProviderUpdateRevoke(p) => set.push(&p.pro_tx_hash.to_lendian()),
        // The refund payments are literal outputs, already in the base filter.
        SpecialPayload::ProviderDissolve(p) => set.push(&p.pro_tx_hash.to_lendian()),
        SpecialPayload::ProviderUpdateShare(p) => {
            set.push(&p.pro_tx_hash.to_lendian());
            add_script(set, p.script_reward.as_bytes());
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            set.push(&p.pro_tx_hash.to_lendian());
            set.push(p.key_id_voting.as_bytes());
        }
        SpecialPayload::AssetLock(p) => {
            // The credit outputs, which are not transaction outputs; an `OP_RETURN` one is not
            // spendable.
            for output in &p.credit_outputs {
                add_output_script(set, output.script_pubkey.as_bytes());
            }
        }
        SpecialPayload::UnknownProTxVersion {
            ..
        }
        | SpecialPayload::CoinbaseCommitment(_)
        | SpecialPayload::QuorumCommitment(_)
        | SpecialPayload::MnhfSignal(_)
        | SpecialPayload::AssetUnlock(_)
        | SpecialPayload::Unknown {
            ..
        } => {}
    }
}

/// Adds everything Core's basic filter takes from a block: for each transaction its output
/// scripts and special-transaction elements, and for each input of every transaction after the
/// coinbase the script of the output it spends, which `script_for_coin` looks up.
///
/// # Errors
///
/// The first error of `script_for_coin`.
pub fn add_block_elements<'a, S, E>(
    set: &mut FilterQuery,
    txs: impl IntoIterator<Item = &'a Transaction>,
    mut script_for_coin: impl FnMut(&OutPoint) -> Result<S, E>,
) -> Result<(), E>
where
    S: AsRef<[u8]>,
{
    for (index, tx) in txs.into_iter().enumerate() {
        add_output_scripts(set, tx);
        add_special_tx_elements(set, tx);
        if index > 0 {
            for input in &tx.inputs {
                add_script(set, script_for_coin(&input.prevout)?.as_ref());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use dash_types::codec::BaseCodec as _;

    use super::*;

    const P2PKH: [u8; 25] = [
        0x76, 0xa9, 0x14, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        0x88, 0xac,
    ];

    /// A transaction of `tx_type` without inputs or outputs, carrying `payload`.
    fn special_tx(tx_type: u16, payload: &[u8]) -> Transaction {
        let mut raw = vec![3, 0];
        raw.extend(tx_type.to_le_bytes());
        raw.extend([0, 0, 0, 0, 0, 0]); // no inputs, no outputs, lock time
        assert!(payload.len() < 253);
        raw.push(payload.len() as u8);
        raw.extend(payload);
        Transaction::decode(&mut raw.as_slice()).unwrap()
    }

    fn elements_of(tx: &Transaction) -> FilterQuery {
        let mut set = FilterQuery::new();
        add_special_tx_elements(&mut set, tx);
        set
    }

    /// A ProUpRevTx payload: version, ProTx hash, reason, inputs hash and the null 96-byte signature.
    fn pro_up_rev_payload() -> Vec<u8> {
        let mut payload = vec![1, 0];
        payload.extend([0x11; 32]);
        payload.extend([0, 0]);
        payload.extend([0x22; 32]);
        payload.extend([0; 96]);
        payload
    }

    #[test]
    fn a_payload_that_ends_early_or_late_adds_nothing() {
        let mut payload = pro_up_rev_payload();
        assert_eq!(elements_of(&special_tx(4, &payload)).len(), 1);
        // Core's `GetTxPayload` refuses a payload with bytes after the fields.
        payload.push(0);
        assert!(elements_of(&special_tx(4, &payload)).is_empty());
        payload.truncate(payload.len() - 2);
        assert!(elements_of(&special_tx(4, &payload)).is_empty());
    }

    #[test]
    fn asset_lock_credit_outputs_skip_op_return_and_empty_scripts() {
        let mut payload = vec![1, 3];
        for script in [&P2PKH[..], &[0x6a, 0x01, 0xaa][..], &[][..]] {
            payload.extend(1000i64.to_le_bytes());
            payload.push(script.len() as u8);
            payload.extend(script);
        }
        let set = elements_of(&special_tx(8, &payload));
        assert_eq!(set.len(), 1);
        let mut expected = FilterQuery::new();
        expected.push(&P2PKH);
        assert_eq!(set, expected);
    }
}
