//! Sigop counting: Core's two `CScript::GetSigOpCount` overloads (`script/script.cpp:154-199`)
//! and the transaction-level counts (`consensus/tx_verify.cpp:118-160`). CS3 owns the limits
//! (DEC-25(1)).

use dash_primitives::Transaction;

use super::flags::ScriptFlags;
use super::interpreter::MAX_PUBKEYS_PER_MULTISIG;
use super::opcodes::*;
use super::parse::{get_op, is_pay_to_script_hash};
use crate::primitives::canonical::is_coinbase;

// Core: script/script.cpp:154 CScript::GetSigOpCount(bool fAccurate)
/// Counts signature operations without executing anything, stopping at the first instruction that
/// does not parse.
///
/// `CHECKSIG(VERIFY)` counts 1. `CHECKMULTISIG(VERIFY)` counts the key count pushed just before it
/// (`OP_1..OP_16`) when `accurate`, otherwise 20. `OP_CHECKDATASIG(VERIFY)` counts 0 (quirk 4).
pub fn script_sigop_count(script: &[u8], accurate: bool) -> u32 {
    let mut n: u32 = 0;
    let mut pc = 0;
    let mut last_opcode = OP_INVALIDOPCODE;
    // `get_op` is `None` at the end too, so this is Core's `pc < end()` loop that breaks on a
    // failed `GetOp`, with one end test: some codegen-unit splits kept both (T30 PF0
    // connect-serial-multisig-250-av-on).
    while let Some(op) = get_op(script, &mut pc) {
        match op.opcode {
            OP_CHECKSIG | OP_CHECKSIGVERIFY => n = n.wrapping_add(1),
            OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                let keys = if accurate && (OP_1..=OP_16).contains(&last_opcode) {
                    decode_op_n(last_opcode)
                } else {
                    MAX_PUBKEYS_PER_MULTISIG as u32
                };
                n = n.wrapping_add(keys);
            }
            _ => {}
        }
        last_opcode = op.opcode;
    }
    n
}

// Core: script/script.cpp:178 CScript::GetSigOpCount(const CScript& scriptSig)
/// `script_pubkey.GetSigOpCount(script_sig)`. **Not** a P2SH-only function (CS1 §4.10):
///
/// 1. A `script_pubkey` that is not P2SH returns its own accurate count; `script_sig` is not read.
/// 2. For P2SH, `script_sig` is walked: a parse failure or any opcode above `OP_16` gives 0.
///    Otherwise the data of the **last instruction** is counted accurately as the redeem script.
///    Core clears that data on every parser call, so a trailing `OP_0`, `OP_1NEGATE`,
///    `OP_RESERVED` or `OP_1..OP_16` gives 0 even after a push, as does an empty `script_sig`.
pub fn script_sigop_count_with_sig(script_pubkey: &[u8], script_sig: &[u8]) -> u32 {
    if !is_pay_to_script_hash(script_pubkey) {
        return script_sigop_count(script_pubkey, true);
    }
    // This is a pay-to-script-hash scriptPubKey: take the last item the scriptSig pushes.
    let mut pc = 0;
    let mut data: &[u8] = &[];
    while pc < script_sig.len() {
        let Some(op) = get_op(script_sig, &mut pc) else {
            return 0;
        };
        if op.opcode > OP_16 {
            return 0;
        }
        data = op.data;
    }
    script_sigop_count(data, true)
}

// Core: consensus/tx_verify.cpp:118 GetLegacySigOpCount
/// The inaccurate count over every scriptSig and every output script.
pub fn legacy_sigop_count(tx: &Transaction) -> u32 {
    let mut n: u32 = 0;
    for input in &tx.inputs {
        n = n.wrapping_add(script_sigop_count(input.script_sig.as_bytes(), false));
    }
    for output in &tx.outputs {
        n = n.wrapping_add(script_sigop_count(output.script_pubkey.as_bytes(), false));
    }
    n
}

// Core: consensus/tx_verify.cpp:132 GetP2SHSigOpCount
/// The accurate count of the redeem scripts of inputs that spend P2SH outputs; 0 for a coinbase.
/// Inputs spending anything else add nothing (CS1 §4.10).
///
/// `spent_script_pubkey(i)` is the script of the output input `i` spends (Core's
/// `AccessCoin(vin[i].prevout).out.scriptPubKey`); the caller guarantees the coin exists.
pub fn p2sh_sigop_count<'s>(
    tx: &Transaction,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8],
) -> u32 {
    if is_coinbase(tx) {
        return 0;
    }
    let mut n: u32 = 0;
    for (i, input) in tx.inputs.iter().enumerate() {
        let prevout = spent_script_pubkey(i);
        if is_pay_to_script_hash(prevout) {
            n = n.wrapping_add(script_sigop_count_with_sig(prevout, input.script_sig.as_bytes()));
        }
    }
    n
}

// Core: consensus/tx_verify.cpp:149 GetTransactionSigOpCount
/// [`legacy_sigop_count`], plus [`p2sh_sigop_count`] when `flags` has P2SH and the transaction is
/// not a coinbase.
pub fn transaction_sigop_count<'s>(
    tx: &Transaction,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8],
    flags: ScriptFlags,
) -> u32 {
    let n = legacy_sigop_count(tx);
    if is_coinbase(tx) {
        return n;
    }
    if flags.contains(ScriptFlags::P2SH) {
        return n.wrapping_add(p2sh_sigop_count(tx, spent_script_pubkey));
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::parse::{p2sh_script_pubkey, push_data};

    fn p2sh() -> Vec<u8> {
        p2sh_script_pubkey(&[0; 20])
    }

    #[test]
    fn checkdatasig_counts_nothing() {
        assert_eq!(script_sigop_count(&[OP_CHECKDATASIG, OP_CHECKDATASIGVERIFY], true), 0);
        assert_eq!(script_sigop_count(&[OP_CHECKSIG, OP_CHECKSIGVERIFY], false), 2);
    }

    #[test]
    fn multisig_counts_the_preceding_small_integer_when_accurate() {
        let script = [OP_2, OP_CHECKMULTISIG, OP_16, OP_CHECKMULTISIGVERIFY, OP_CHECKMULTISIG];
        assert_eq!(script_sigop_count(&script, true), 2 + 16 + 20);
        assert_eq!(script_sigop_count(&script, false), 60);
        // OP_0 is not OP_1..OP_16: 20.
        assert_eq!(script_sigop_count(&[OP_0, OP_CHECKMULTISIG], true), 20);
    }

    #[test]
    fn counting_stops_at_a_parse_failure() {
        assert_eq!(script_sigop_count(&[OP_CHECKSIG, 0x05, 0xaa, OP_CHECKSIG], true), 1);
    }

    #[test]
    fn non_p2sh_falls_back_to_the_accurate_count() {
        // The scriptSig is not even read: OP_RETURN does not matter.
        assert_eq!(script_sigop_count_with_sig(&[OP_2, OP_CHECKMULTISIG], &[OP_RETURN]), 2);
    }

    #[test]
    fn p2sh_counts_the_last_instructions_data() {
        let spk = p2sh();
        let mut sig = Vec::new();
        push_data(&mut sig, &[OP_CHECKSIG]);
        assert_eq!(script_sigop_count_with_sig(&spk, &sig), 1);
        // A trailing small integer clears the data.
        let mut trailing = sig.clone();
        trailing.push(OP_1);
        assert_eq!(script_sigop_count_with_sig(&spk, &trailing), 0);
        trailing.pop();
        trailing.push(OP_RESERVED);
        assert_eq!(script_sigop_count_with_sig(&spk, &trailing), 0);
        // Empty, non-push and truncated scriptSigs.
        assert_eq!(script_sigop_count_with_sig(&spk, &[]), 0);
        assert_eq!(script_sigop_count_with_sig(&spk, &[OP_NOP]), 0);
        assert_eq!(script_sigop_count_with_sig(&spk, &[0x02, OP_CHECKSIG]), 0);
    }
}
