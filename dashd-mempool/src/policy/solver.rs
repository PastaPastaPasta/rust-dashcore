//! Core's standard output classifier and its solution vectors.
// Keep aligned with dashd-rpc/src/sign.rs::solver_solutions (and its helpers in
// descriptor.rs/key_io.rs) until the shared dash-consensus classifier is scheduled (MP1a Q7).

use dash_consensus::script::interpreter::MAX_PUBKEYS_PER_MULTISIG;
use dash_consensus::script::opcodes::*;
use dash_consensus::script::{
    DEFAULT_MAX_NUM_SIZE, Op, PubKeyRef, ScriptNum, check_minimal_push, get_op,
    is_pay_to_script_hash, is_push_only,
};

/// Core's `TxoutType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxoutType {
    Nonstandard,
    Pubkey,
    PubkeyHash,
    ScriptHash,
    Multisig,
    NullData,
}

/// `Solver`: the type and `vSolutions`, including one-byte multisig counts.
// Core v24.0.0: script/standard.cpp:43-164
pub fn solver(script: &[u8]) -> (TxoutType, Vec<Vec<u8>>) {
    if is_pay_to_script_hash(script) {
        return (TxoutType::ScriptHash, vec![script[2..22].to_vec()]);
    }
    if script.first() == Some(&OP_RETURN) && is_push_only(&script[1..]) {
        return (TxoutType::NullData, Vec::new());
    }
    if let [n @ (33 | 65), key @ .., OP_CHECKSIG] = script
        && usize::from(*n) == key.len()
        && PubKeyRef::new(key).is_some()
    {
        return (TxoutType::Pubkey, vec![key.to_vec()]);
    }
    if let [OP_DUP, OP_HASH160, 20, hash @ .., OP_EQUALVERIFY, OP_CHECKSIG] = script
        && hash.len() == 20
    {
        return (TxoutType::PubkeyHash, vec![hash.to_vec()]);
    }
    if let Some(solutions) = match_multisig(script) {
        return (TxoutType::Multisig, solutions);
    }
    (TxoutType::Nonstandard, Vec::new())
}

// Core v24.0.0: script/standard.cpp:69-95 GetMultisigKeyCount
fn multisig_key_count(op: Op<'_>) -> Option<u8> {
    let count = if (OP_1..=OP_16).contains(&op.opcode) {
        i32::from(op.opcode - OP_1 + 1)
    } else if op.opcode <= OP_PUSHDATA4 && check_minimal_push(op.data, op.opcode) {
        ScriptNum::decode(op.data, true, DEFAULT_MAX_NUM_SIZE).ok()?.getint()
    } else {
        return None;
    };
    (1..=MAX_PUBKEYS_PER_MULTISIG).contains(&count).then_some(count as u8)
}

// Core v24.0.0: script/standard.cpp:97-117 MatchMultisig
fn match_multisig(script: &[u8]) -> Option<Vec<Vec<u8>>> {
    if script.last() != Some(&OP_CHECKMULTISIG) {
        return None;
    }
    let mut pc = 0;
    let required = multisig_key_count(get_op(script, &mut pc)?)?;
    let mut solutions = vec![vec![required]];
    loop {
        let op = get_op(script, &mut pc)?;
        if PubKeyRef::new(op.data).is_some() {
            solutions.push(op.data.to_vec());
            continue;
        }
        let count = multisig_key_count(op)?;
        if solutions.len() - 1 != usize::from(count) || count < required || pc + 1 != script.len() {
            return None;
        }
        solutions.push(vec![count]);
        return Some(solutions);
    }
}
