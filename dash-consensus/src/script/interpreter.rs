//! Core's script interpreter: `EvalScript`, `VerifyScript` and `CastToBool`
//! (`script/interpreter.cpp`), with the DIP0020 opcodes and `OP_CHECKDATASIG(VERIFY)`.
//!
//! The port follows Core's evaluation order step for step (CS1 §4.5). Where Core throws (a script
//! number that is too long or not minimal, `stacktop` out of range, `popstack` on an empty stack)
//! and its `catch (...)` reports `SCRIPT_ERR_UNKNOWN_ERROR`, the port returns
//! [`ScriptError::UnknownError`] at the same point. On every error the stack is left exactly as
//! Core leaves it.

use super::checker::SignatureChecker;
use super::ecdsa::{PubKeyRef, verify_lax};
use super::encoding::{check_pubkey_encoding, check_signature_encoding};
use super::error::ScriptError;
use super::flags::ScriptFlags;
use super::num::{DEFAULT_MAX_NUM_SIZE, ScriptNum, ScriptNumError};
use super::opcodes::*;
use super::parse::{
    check_minimal_push, find_and_delete, get_op, is_pay_to_script_hash, is_push_only, push_data,
};
use crate::hash;

/// `MAX_SCRIPT_ELEMENT_SIZE`: the largest push or stack element.
pub const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;
/// `MAX_OPS_PER_SCRIPT`: non-push opcodes per script, plus each `CHECKMULTISIG`'s key count.
pub const MAX_OPS_PER_SCRIPT: i32 = 201;
/// `MAX_PUBKEYS_PER_MULTISIG`.
pub const MAX_PUBKEYS_PER_MULTISIG: i32 = 20;
/// `MAX_SCRIPT_SIZE`.
pub const MAX_SCRIPT_SIZE: usize = 10_000;
/// `MAX_STACK_SIZE`: stack plus altstack elements after any opcode.
pub const MAX_STACK_SIZE: usize = 1000;
/// `LOCKTIME_THRESHOLD` (`script/script.h`, an `unsigned int`): lock times below it are heights,
/// at or above it UNIX times.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;
/// `CTxIn::SEQUENCE_LOCKTIME_DISABLE_FLAG`: the sequence carries no relative lock time.
pub const SEQUENCE_LOCKTIME_DISABLE_FLAG: u32 = 1 << 31;
/// `CTxIn::SEQUENCE_LOCKTIME_TYPE_FLAG`: the relative lock time counts 512-second units.
pub const SEQUENCE_LOCKTIME_TYPE_FLAG: u32 = 1 << 22;
/// `CTxIn::SEQUENCE_LOCKTIME_MASK`.
pub const SEQUENCE_LOCKTIME_MASK: u32 = 0x0000_ffff;
/// `CTxIn::SEQUENCE_FINAL`.
pub const SEQUENCE_FINAL: u32 = 0xffff_ffff;

/// Core's `std::vector<valtype>`: the top of the stack is the last element.
pub type Stack = Vec<Vec<u8>>;

impl From<ScriptNumError> for ScriptError {
    /// Core's `catch (...)` turns a `scriptnum_error` into `SCRIPT_ERR_UNKNOWN_ERROR`.
    fn from(_: ScriptNumError) -> Self {
        ScriptError::UnknownError
    }
}

// Core: script/interpreter.cpp:35 CastToBool
/// Any non-zero byte is true, except negative zero (only the sign bit set in the last byte).
pub fn cast_to_bool(v: &[u8]) -> bool {
    for (i, &b) in v.iter().enumerate() {
        if b != 0 {
            // Can be negative zero.
            return !(i == v.len() - 1 && b == 0x80);
        }
    }
    false
}

/// `stacktop(-depth)`: Core's `stack.at(stack.size() - depth)`, which throws when out of range.
fn top(stack: &[Vec<u8>], depth: usize) -> Result<&Vec<u8>, ScriptError> {
    // census: unreachable (every caller checks the stack size first, as in Core)
    stack.len().checked_sub(depth).and_then(|i| stack.get(i)).ok_or(ScriptError::UnknownError)
}

fn top_mut(stack: &mut [Vec<u8>], depth: usize) -> Result<&mut Vec<u8>, ScriptError> {
    // census: unreachable (every caller checks the stack size first, as in Core)
    let i = stack.len().checked_sub(depth).ok_or(ScriptError::UnknownError)?;
    // census: unreachable (as above)
    stack.get_mut(i).ok_or(ScriptError::UnknownError)
}

/// `popstack`: throws on an empty stack.
fn pop(stack: &mut Stack) -> Result<Vec<u8>, ScriptError> {
    // census: unreachable (every caller checks the stack size first, as in Core)
    stack.pop().ok_or(ScriptError::UnknownError)
}

/// `vchTrue` (`01`) or `vchFalse` (empty).
fn push_bool(stack: &mut Stack, value: bool) {
    stack.push(if value {
        vec![1]
    } else {
        Vec::new()
    });
}

fn num(vch: &[u8], require_minimal: bool) -> Result<ScriptNum, ScriptError> {
    Ok(ScriptNum::decode(vch, require_minimal, DEFAULT_MAX_NUM_SIZE)?)
}

/// Core's O(1) condition stack (`interpreter.cpp:268`): the size of the implied stack of branch
/// booleans and the position of its first false value.
#[derive(Default)]
struct ConditionStack {
    size: u32,
    first_false: Option<u32>,
}

impl ConditionStack {
    fn is_empty(&self) -> bool {
        self.size == 0
    }

    fn all_true(&self) -> bool {
        self.first_false.is_none()
    }

    fn push(&mut self, value: bool) {
        if self.first_false.is_none() && !value {
            // The stack consists of all true values, and a false is added.
            self.first_false = Some(self.size);
        }
        self.size += 1;
    }

    /// Only called on a non-empty stack (the interpreter checks first; Core asserts).
    fn pop(&mut self) {
        debug_assert!(self.size > 0);
        self.size -= 1;
        if self.first_false == Some(self.size) {
            // Popping off the first false value makes everything true.
            self.first_false = None;
        }
    }

    /// Only called on a non-empty stack (the interpreter checks first; Core asserts).
    fn toggle_top(&mut self) {
        debug_assert!(self.size > 0);
        match self.first_false {
            // The current stack is all true values; the first false will be the top.
            None => self.first_false = Some(self.size - 1),
            // The top is the first false value; toggling it makes everything true.
            Some(pos) if pos == self.size - 1 => self.first_false = None,
            // A false value lower down: toggling anything but the first false is unobservable.
            Some(_) => {}
        }
    }
}

// Core: script/interpreter.cpp:321 EvalChecksig
/// `OP_CHECKSIG(VERIFY)`'s check: delete the signature from the scriptCode, check the encodings,
/// ask the checker. `Err` fails the script; `Ok(false)` is a failed signature check.
fn eval_checksig<C: SignatureChecker + ?Sized>(
    sig: &[u8],
    pubkey: &[u8],
    script_code: &[u8],
    flags: ScriptFlags,
    checker: &C,
) -> Result<bool, ScriptError> {
    // Subset of script starting at the most recent codeseparator.
    let mut script_code = script_code.to_vec();
    // Drop the signature, since there's no way for a signature to sign itself.
    let mut pattern = Vec::with_capacity(sig.len() + 5);
    push_data(&mut pattern, sig);
    if find_and_delete(&mut script_code, &pattern) > 0
        && flags.contains(ScriptFlags::CONST_SCRIPTCODE)
    {
        return Err(ScriptError::SigFindAndDelete);
    }
    check_signature_encoding(sig, flags)?;
    check_pubkey_encoding(pubkey, flags)?;
    let success = checker.check_sig(sig, pubkey, &script_code);
    if !success && flags.contains(ScriptFlags::NULLFAIL) && !sig.is_empty() {
        return Err(ScriptError::SigNullFail);
    }
    Ok(success)
}

// Core: script/interpreter.cpp:346 EvalScript
/// Evaluates `script` on `stack` (in and out, as in Core) with `SigVersion::BASE`.
///
/// On error `stack` holds exactly Core's stack at the failing opcode.
pub fn eval_script<C: SignatureChecker + ?Sized>(
    stack: &mut Stack,
    script: &[u8],
    flags: ScriptFlags,
    checker: &C,
) -> Result<(), ScriptError> {
    use ScriptError::*;

    if script.len() > MAX_SCRIPT_SIZE {
        return Err(ScriptSize);
    }
    let mut pc = 0;
    let mut pbegincodehash = 0;
    let mut exec = ConditionStack::default();
    let mut altstack: Stack = Vec::new();
    let mut op_count = 0i32;
    let require_minimal = flags.contains(ScriptFlags::MINIMALDATA);

    while pc < script.len() {
        let f_exec = exec.all_true();

        // Read instruction.
        let Some(op) = get_op(script, &mut pc) else {
            return Err(BadOpcode);
        };
        let opcode = op.opcode;
        if op.data.len() > MAX_SCRIPT_ELEMENT_SIZE {
            return Err(PushSize);
        }

        // Note how OP_RESERVED does not count towards the opcode limit.
        if opcode > OP_16 {
            op_count += 1;
            if op_count > MAX_OPS_PER_SCRIPT {
                return Err(OpCount);
            }
        }

        // Disabled opcodes (CVE-2010-5137), even in an unexecuted branch.
        if matches!(opcode, OP_INVERT | OP_2MUL | OP_2DIV | OP_MUL | OP_LSHIFT | OP_RSHIFT) {
            return Err(DisabledOpcode);
        }

        // With CONST_SCRIPTCODE, OP_CODESEPARATOR is rejected even in an unexecuted branch.
        if opcode == OP_CODESEPARATOR && flags.contains(ScriptFlags::CONST_SCRIPTCODE) {
            return Err(OpCodeSeparator);
        }

        if f_exec || (OP_IF..=OP_ENDIF).contains(&opcode) {
            checker.observe_instruction(opcode);
        }
        if f_exec && opcode <= OP_PUSHDATA4 {
            if require_minimal && !check_minimal_push(op.data, opcode) {
                return Err(MinimalData);
            }
            stack.push(op.data.to_vec());
        } else if f_exec || (OP_IF..=OP_ENDIF).contains(&opcode) {
            match opcode {
                //
                // Push value
                //
                OP_1NEGATE | OP_1..=OP_16 => {
                    // ( -- value)
                    let value = i64::from(opcode) - i64::from(OP_1 - 1);
                    stack.push(ScriptNum::new(value).encode());
                }

                //
                // Control
                //
                OP_NOP => {}

                OP_CHECKLOCKTIMEVERIFY => {
                    // Not enabled: a NOP2, without the DISCOURAGE_UPGRADABLE_NOPS check.
                    if flags.contains(ScriptFlags::CHECKLOCKTIMEVERIFY) {
                        if stack.is_empty() {
                            return Err(InvalidStackOperation);
                        }
                        // 5-byte operands, so lock times reach 2^39-1 (no year-2038 problem).
                        let lock_time = ScriptNum::decode(top(stack, 1)?, require_minimal, 5)?;
                        if lock_time < 0 {
                            return Err(NegativeLocktime);
                        }
                        if !checker.check_lock_time(lock_time) {
                            return Err(UnsatisfiedLocktime);
                        }
                    }
                }

                OP_CHECKSEQUENCEVERIFY => {
                    // Not enabled: a NOP3, without the DISCOURAGE_UPGRADABLE_NOPS check.
                    if flags.contains(ScriptFlags::CHECKSEQUENCEVERIFY) {
                        if stack.is_empty() {
                            return Err(InvalidStackOperation);
                        }
                        let sequence = ScriptNum::decode(top(stack, 1)?, require_minimal, 5)?;
                        if sequence < 0 {
                            return Err(NegativeLocktime);
                        }
                        // The disable flag makes it a NOP, for soft-fork extensibility.
                        let disabled = sequence & i64::from(SEQUENCE_LOCKTIME_DISABLE_FLAG);
                        if disabled != 0 {
                            // break
                        } else if !checker.check_sequence(sequence) {
                            return Err(UnsatisfiedLocktime);
                        }
                    }
                }

                OP_NOP1 | OP_NOP4 | OP_NOP5 | OP_NOP6 | OP_NOP7 | OP_NOP8 | OP_NOP9 | OP_NOP10 => {
                    if flags.contains(ScriptFlags::DISCOURAGE_UPGRADABLE_NOPS) {
                        return Err(DiscourageUpgradableNops);
                    }
                }

                OP_IF | OP_NOTIF => {
                    // <expression> if [statements] [else [statements]] endif
                    let mut value = false;
                    if f_exec {
                        if stack.is_empty() {
                            return Err(UnbalancedConditional);
                        }
                        value = cast_to_bool(top(stack, 1)?);
                        if opcode == OP_NOTIF {
                            value = !value;
                        }
                        pop(stack)?;
                    }
                    exec.push(value);
                }

                OP_ELSE => {
                    if exec.is_empty() {
                        return Err(UnbalancedConditional);
                    }
                    exec.toggle_top();
                }

                OP_ENDIF => {
                    if exec.is_empty() {
                        return Err(UnbalancedConditional);
                    }
                    exec.pop();
                }

                OP_VERIFY => {
                    // (true -- ) or (false -- false) and return
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    if cast_to_bool(top(stack, 1)?) {
                        pop(stack)?;
                    } else {
                        return Err(Verify);
                    }
                }

                OP_RETURN => return Err(OpReturn),

                //
                // Stack ops
                //
                OP_TOALTSTACK => {
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let v = pop(stack)?;
                    altstack.push(v);
                }

                OP_FROMALTSTACK => {
                    let Some(v) = altstack.pop() else {
                        return Err(InvalidAltstackOperation);
                    };
                    stack.push(v);
                }

                OP_2DROP => {
                    // (x1 x2 -- )
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    pop(stack)?;
                    pop(stack)?;
                }

                OP_2DUP | OP_3DUP | OP_2OVER => {
                    // (x1 x2 -- x1 x2 x1 x2), (x1 x2 x3 -- x1 x2 x3 x1 x2 x3),
                    // (x1 x2 x3 x4 -- x1 x2 x3 x4 x1 x2)
                    let (depth, count) = match opcode {
                        OP_2DUP => (2, 2),
                        OP_3DUP => (3, 3),
                        _ /* OP_2OVER */ => (4, 2),
                    };
                    if stack.len() < depth {
                        return Err(InvalidStackOperation);
                    }
                    let from = stack.len() - depth;
                    for i in from..from + count {
                        let v = stack[i].clone();
                        stack.push(v);
                    }
                }

                OP_2ROT => {
                    // (x1 x2 x3 x4 x5 x6 -- x3 x4 x5 x6 x1 x2)
                    if stack.len() < 6 {
                        return Err(InvalidStackOperation);
                    }
                    let len = stack.len();
                    stack[len - 6..].rotate_left(2);
                }

                OP_2SWAP => {
                    // (x1 x2 x3 x4 -- x3 x4 x1 x2)
                    if stack.len() < 4 {
                        return Err(InvalidStackOperation);
                    }
                    let len = stack.len();
                    stack.swap(len - 4, len - 2);
                    stack.swap(len - 3, len - 1);
                }

                OP_IFDUP => {
                    // (x - 0 | x x)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let v = top(stack, 1)?;
                    if cast_to_bool(v) {
                        let v = v.clone();
                        stack.push(v);
                    }
                }

                OP_DEPTH => {
                    // -- stacksize
                    stack.push(ScriptNum::new(stack.len() as i64).encode());
                }

                OP_DROP => {
                    // (x -- )
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    pop(stack)?;
                }

                OP_DUP => {
                    // (x -- x x)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let v = top(stack, 1)?.clone();
                    stack.push(v);
                }

                OP_NIP => {
                    // (x1 x2 -- x2)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let len = stack.len();
                    stack.remove(len - 2);
                }

                OP_OVER => {
                    // (x1 x2 -- x1 x2 x1)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let v = top(stack, 2)?.clone();
                    stack.push(v);
                }

                OP_PICK | OP_ROLL => {
                    // (xn ... x2 x1 x0 n - xn ... x2 x1 x0 xn)
                    // (xn ... x2 x1 x0 n - ... x2 x1 x0 xn)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let n = num(top(stack, 1)?, require_minimal)?.getint();
                    pop(stack)?;
                    if n < 0 || n as usize >= stack.len() {
                        return Err(InvalidStackOperation);
                    }
                    let index = stack.len() - n as usize - 1;
                    let v = if opcode == OP_ROLL {
                        stack.remove(index)
                    } else {
                        stack[index].clone()
                    };
                    stack.push(v);
                }

                OP_ROT => {
                    // (x1 x2 x3 -- x2 x3 x1)
                    if stack.len() < 3 {
                        return Err(InvalidStackOperation);
                    }
                    let len = stack.len();
                    stack.swap(len - 3, len - 2);
                    stack.swap(len - 2, len - 1);
                }

                OP_SWAP => {
                    // (x1 x2 -- x2 x1)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let len = stack.len();
                    stack.swap(len - 2, len - 1);
                }

                OP_TUCK => {
                    // (x1 x2 -- x2 x1 x2)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let v = top(stack, 1)?.clone();
                    let len = stack.len();
                    stack.insert(len - 2, v);
                }

                OP_SIZE => {
                    // (in -- in size)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let size = top(stack, 1)?.len() as i64;
                    stack.push(ScriptNum::new(size).encode());
                }

                //
                // Bitwise logic
                //
                OP_EQUAL | OP_EQUALVERIFY => {
                    // (x1 x2 - bool)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let equal = top(stack, 2)? == top(stack, 1)?;
                    pop(stack)?;
                    pop(stack)?;
                    push_bool(stack, equal);
                    if opcode == OP_EQUALVERIFY {
                        if equal {
                            pop(stack)?;
                        } else {
                            return Err(EqualVerify);
                        }
                    }
                }

                OP_AND | OP_OR | OP_XOR => {
                    // (x1 x2 - out)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    // Inputs must be the same size.
                    if top(stack, 2)?.len() != top(stack, 1)?.len() {
                        return Err(InvalidOperandSize);
                    }
                    // x1 is modified in place, then x2 is popped.
                    let x2 = pop(stack)?;
                    let x1 = top_mut(stack, 1)?;
                    for (a, b) in x1.iter_mut().zip(&x2) {
                        match opcode {
                            OP_AND => *a &= b,
                            OP_OR => *a |= b,
                            _ /* OP_XOR */ => *a ^= b,
                        }
                    }
                }

                //
                // Numeric
                //
                OP_1ADD | OP_1SUB | OP_NEGATE | OP_ABS | OP_NOT | OP_0NOTEQUAL => {
                    // (in -- out)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let bn = num(top(stack, 1)?, require_minimal)?;
                    let bn = match opcode {
                        OP_1ADD => bn + ScriptNum::ONE,
                        OP_1SUB => bn - ScriptNum::ONE,
                        OP_NEGATE => -bn,
                        OP_ABS if bn < 0 => -bn,
                        OP_ABS => bn,
                        OP_NOT => ScriptNum::from(bn == 0),
                        _ /* OP_0NOTEQUAL */ => ScriptNum::from(bn != 0),
                    };
                    pop(stack)?;
                    stack.push(bn.encode());
                }

                OP_ADD
                | OP_SUB
                | OP_DIV
                | OP_MOD
                | OP_BOOLAND
                | OP_BOOLOR
                | OP_NUMEQUAL
                | OP_NUMEQUALVERIFY
                | OP_NUMNOTEQUAL
                | OP_LESSTHAN
                | OP_GREATERTHAN
                | OP_LESSTHANOREQUAL
                | OP_GREATERTHANOREQUAL
                | OP_MIN
                | OP_MAX => {
                    // (x1 x2 -- out)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let bn1 = num(top(stack, 2)?, require_minimal)?;
                    let bn2 = num(top(stack, 1)?, require_minimal)?;
                    let bn = match opcode {
                        OP_ADD => bn1 + bn2,
                        OP_SUB => bn1 - bn2,
                        OP_DIV => {
                            // Denominator must not be 0.
                            if bn2 == 0 {
                                return Err(DivByZero);
                            }
                            bn1 / bn2
                        }
                        OP_MOD => {
                            // Divisor must not be 0.
                            if bn2 == 0 {
                                return Err(ModByZero);
                            }
                            bn1 % bn2
                        }
                        OP_BOOLAND => ScriptNum::from(bn1 != 0 && bn2 != 0),
                        OP_BOOLOR => ScriptNum::from(bn1 != 0 || bn2 != 0),
                        OP_NUMEQUAL | OP_NUMEQUALVERIFY => ScriptNum::from(bn1 == bn2),
                        OP_NUMNOTEQUAL => ScriptNum::from(bn1 != bn2),
                        OP_LESSTHAN => ScriptNum::from(bn1 < bn2),
                        OP_GREATERTHAN => ScriptNum::from(bn1 > bn2),
                        OP_LESSTHANOREQUAL => ScriptNum::from(bn1 <= bn2),
                        OP_GREATERTHANOREQUAL => ScriptNum::from(bn1 >= bn2),
                        OP_MIN => bn1.min(bn2),
                        _ /* OP_MAX */ => bn1.max(bn2),
                    };
                    pop(stack)?;
                    pop(stack)?;
                    stack.push(bn.encode());

                    if opcode == OP_NUMEQUALVERIFY {
                        if cast_to_bool(top(stack, 1)?) {
                            pop(stack)?;
                        } else {
                            return Err(NumEqualVerify);
                        }
                    }
                }

                OP_WITHIN => {
                    // (x min max -- out)
                    if stack.len() < 3 {
                        return Err(InvalidStackOperation);
                    }
                    let bn1 = num(top(stack, 3)?, require_minimal)?;
                    let bn2 = num(top(stack, 2)?, require_minimal)?;
                    let bn3 = num(top(stack, 1)?, require_minimal)?;
                    let value = bn2 <= bn1 && bn1 < bn3;
                    pop(stack)?;
                    pop(stack)?;
                    pop(stack)?;
                    push_bool(stack, value);
                }

                //
                // Crypto
                //
                OP_RIPEMD160 | OP_SHA1 | OP_SHA256 | OP_HASH160 | OP_HASH256 => {
                    // (in -- hash)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let v = top(stack, 1)?;
                    let digest = match opcode {
                        OP_RIPEMD160 => hash::ripemd160(v).to_vec(),
                        OP_SHA1 => hash::sha1(v).to_vec(),
                        OP_SHA256 => hash::sha256(v).to_vec(),
                        OP_HASH160 => hash::hash160(v).to_vec(),
                        _ /* OP_HASH256 */ => hash::sha256d(v).to_vec(),
                    };
                    pop(stack)?;
                    stack.push(digest);
                }

                OP_CODESEPARATOR => {
                    // Hash starts after the code separator.
                    pbegincodehash = pc;
                }

                OP_CHECKSIG | OP_CHECKSIGVERIFY => {
                    // (sig pubkey -- bool)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let success = eval_checksig(
                        top(stack, 2)?,
                        top(stack, 1)?,
                        &script[pbegincodehash..],
                        flags,
                        checker,
                    )?;
                    pop(stack)?;
                    pop(stack)?;
                    push_bool(stack, success);
                    if opcode == OP_CHECKSIGVERIFY {
                        if success {
                            pop(stack)?;
                        } else {
                            return Err(CheckSigVerify);
                        }
                    }
                }

                OP_CHECKDATASIG | OP_CHECKDATASIGVERIFY => {
                    // (sig message pubkey -- bool)
                    if stack.len() < 3 {
                        return Err(InvalidStackOperation);
                    }
                    let (sig, message, pubkey) = (top(stack, 3)?, top(stack, 2)?, top(stack, 1)?);
                    check_signature_encoding(sig, flags)?;
                    check_pubkey_encoding(pubkey, flags)?;
                    // No checker and no signature cache: Core calls CPubKey::Verify directly, on
                    // the single SHA-256 of the message and the whole signature (quirk 4).
                    let success = !sig.is_empty()
                        && PubKeyRef::new(pubkey)
                            .is_some_and(|key| verify_lax(key, &hash::sha256(message), sig));
                    if !success && flags.contains(ScriptFlags::NULLFAIL) && !sig.is_empty() {
                        return Err(SigNullFail);
                    }
                    pop(stack)?;
                    pop(stack)?;
                    pop(stack)?;
                    push_bool(stack, success);
                    if opcode == OP_CHECKDATASIGVERIFY {
                        if success {
                            pop(stack)?;
                        } else {
                            return Err(CheckDataSigVerify);
                        }
                    }
                }

                OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                    // ([sig ...] num_of_signatures [pubkey ...] num_of_pubkeys -- bool)
                    let success = eval_checkmultisig(
                        stack,
                        &script[pbegincodehash..],
                        flags,
                        checker,
                        require_minimal,
                        &mut op_count,
                    )?;
                    push_bool(stack, success);
                    if opcode == OP_CHECKMULTISIGVERIFY {
                        if success {
                            pop(stack)?;
                        } else {
                            return Err(CheckMultisigVerify);
                        }
                    }
                }

                //
                // Splice operations (DIP0020)
                //
                OP_CAT => {
                    // (x1 x2 -- out)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    if top(stack, 2)?.len() + top(stack, 1)?.len() > MAX_SCRIPT_ELEMENT_SIZE {
                        return Err(PushSize);
                    }
                    let x2 = pop(stack)?;
                    top_mut(stack, 1)?.extend_from_slice(&x2);
                }

                OP_SPLIT => {
                    // (in position -- x1 x2)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    let position = i64::from(num(top(stack, 1)?, require_minimal)?.getint());
                    // A position below 0 or past the end of the input is an error. Core copies
                    // the input first; the copy cannot fail, so checking first is the same.
                    if position < 0 || position as usize > top(stack, 2)?.len() {
                        return Err(InvalidSplitRange);
                    }
                    pop(stack)?;
                    let mut x1 = pop(stack)?;
                    let x2 = x1.split_off(position as usize);
                    stack.push(x1);
                    stack.push(x2);
                }

                //
                // Conversion operations (DIP0020)
                //
                OP_NUM2BIN => {
                    // (in size -- out)
                    if stack.len() < 2 {
                        return Err(InvalidStackOperation);
                    }
                    // Core assigns the int to a uint64_t, so a negative size becomes huge.
                    let size = i64::from(num(top(stack, 1)?, require_minimal)?.getint()) as u64;
                    if size > MAX_SCRIPT_ELEMENT_SIZE as u64 {
                        return Err(PushSize);
                    }
                    pop(stack)?;
                    let size = size as usize;
                    let rawnum = top_mut(stack, 1)?;
                    // Try to see if we can fit that number in the number of bytes requested.
                    ScriptNum::minimally_encode(rawnum);
                    if rawnum.len() > size {
                        // We definitively cannot.
                        return Err(ImpossibleEncoding);
                    }
                    // Already the right size: nothing to do.
                    if rawnum.len() < size {
                        let mut signbit = 0x00;
                        if let Some(last) = rawnum.last_mut() {
                            signbit = *last & 0x80;
                            *last &= 0x7f;
                        }
                        rawnum.resize(size - 1, 0x00);
                        rawnum.push(signbit);
                    }
                }

                OP_BIN2NUM => {
                    // (in -- out)
                    if stack.is_empty() {
                        return Err(InvalidStackOperation);
                    }
                    let n = top_mut(stack, 1)?;
                    ScriptNum::minimally_encode(n);
                    // The result must be a valid 4-byte number.
                    if !ScriptNum::is_minimally_encoded(n, DEFAULT_MAX_NUM_SIZE) {
                        return Err(InvalidNumberRange);
                    }
                }

                _ => return Err(BadOpcode),
            }
        }

        // Size limits.
        if stack.len() + altstack.len() > MAX_STACK_SIZE {
            return Err(StackSize);
        }
    }

    if !exec.is_empty() {
        return Err(UnbalancedConditional);
    }
    Ok(())
}

// Core: script/interpreter.cpp:1091 OP_CHECKMULTISIG / OP_CHECKMULTISIGVERIFY
/// The body of `OP_CHECKMULTISIG(VERIFY)` up to the result push (CS1 §4.5's eleven steps).
/// Returns whether the operation succeeded; the caller pushes it. Indices count from the top of
/// the stack, 1 being the top, as Core's `stacktop(-i)`.
fn eval_checkmultisig<C: SignatureChecker + ?Sized>(
    stack: &mut Stack,
    script_code: &[u8],
    flags: ScriptFlags,
    checker: &C,
    require_minimal: bool,
    op_count: &mut i32,
) -> Result<bool, ScriptError> {
    use ScriptError::*;

    let mut i = 1i32;
    if (stack.len() as i32) < i {
        return Err(InvalidStackOperation);
    }

    let mut keys = num(top(stack, i as usize)?, require_minimal)?.getint();
    if !(0..=MAX_PUBKEYS_PER_MULTISIG).contains(&keys) {
        return Err(PubkeyCount);
    }
    // The declared key count is charged against the opcode limit (CS1 §4.5).
    *op_count += keys;
    if *op_count > MAX_OPS_PER_SCRIPT {
        return Err(OpCount);
    }
    i += 1;
    let mut ikey = i;
    // ikey2 is the position of the last non-signature item in the stack (top item = 1). With
    // NULLFAIL it drives the cleanup if the operation fails.
    let mut ikey2 = keys + 2;
    i += keys;
    if (stack.len() as i32) < i {
        return Err(InvalidStackOperation);
    }

    let mut sigs = num(top(stack, i as usize)?, require_minimal)?.getint();
    if sigs < 0 || sigs > keys {
        return Err(SigCount);
    }
    i += 1;
    let mut isig = i;
    i += sigs;
    if (stack.len() as i32) < i {
        return Err(InvalidStackOperation);
    }

    // Subset of script starting at the most recent codeseparator.
    let mut script_code = script_code.to_vec();

    // Drop the signatures, since there's no way for a signature to sign itself. Each deletion
    // works on the previous one's output.
    for k in 0..sigs {
        let mut pattern = Vec::new();
        push_data(&mut pattern, top(stack, (isig + k) as usize)?);
        if find_and_delete(&mut script_code, &pattern) > 0
            && flags.contains(ScriptFlags::CONST_SCRIPTCODE)
        {
            return Err(SigFindAndDelete);
        }
    }

    let mut success = true;
    while success && sigs > 0 {
        let sig = top(stack, isig as usize)?;
        let pubkey = top(stack, ikey as usize)?;

        // The exact order of pubkey/signature evaluation is observable through CHECKMULTISIG NOT
        // under STRICTENC.
        check_signature_encoding(sig, flags)?;
        check_pubkey_encoding(pubkey, flags)?;

        // Check signature.
        if checker.check_sig(sig, pubkey, &script_code) {
            isig += 1;
            sigs -= 1;
        }
        ikey += 1;
        keys -= 1;

        // If there are more signatures left than keys left, then too many signatures have
        // failed. Exit early, without checking any further signatures.
        if sigs > keys {
            success = false;
        }
    }

    // Clean up the stack of actual arguments (`while (i-- > 1)`).
    while i > 1 {
        i -= 1;
        // If the operation failed, NULLFAIL requires every signature to be the empty vector.
        if !success
            && flags.contains(ScriptFlags::NULLFAIL)
            && ikey2 == 0
            && !top(stack, 1)?.is_empty()
        {
            return Err(SigNullFail);
        }
        if ikey2 > 0 {
            ikey2 -= 1;
        }
        pop(stack)?;
    }

    // A bug causes CHECKMULTISIG to consume one extra argument whose contents were not checked in
    // any way. NULLDUMMY requires it to be empty.
    if stack.is_empty() {
        // census: unreachable (the signature-stack check above already counts the dummy's slot)
        return Err(InvalidStackOperation);
    }
    if flags.contains(ScriptFlags::NULLDUMMY) && !top(stack, 1)?.is_empty() {
        return Err(SigNullDummy);
    }
    pop(stack)?;
    Ok(success)
}

// Core: script/interpreter.cpp:1652 VerifyScript
/// Evaluates `script_sig` and then `script_pubkey` on one stack, then, under `P2SH` and for a P2SH
/// `script_pubkey`, the redeem script on the stack the scriptSig left, then `CLEANSTACK`.
///
/// # Panics
///
/// If `flags` has `CLEANSTACK` without `P2SH` and evaluation gets as far as the clean-stack check,
/// which is where Core asserts (`interpreter.cpp:1710`). Callers never pass that combination (see
/// [`ScriptFlags::is_valid_combination`]).
pub fn verify_script<C: SignatureChecker + ?Sized>(
    script_sig: &[u8],
    script_pubkey: &[u8],
    flags: ScriptFlags,
    checker: &C,
) -> Result<(), ScriptError> {
    use ScriptError::*;

    if flags.contains(ScriptFlags::SIGPUSHONLY) && !is_push_only(script_sig) {
        return Err(SigPushOnly);
    }

    // scriptSig and scriptPubKey are evaluated in sequence on the same stack, not concatenated
    // (CVE-2010-5141).
    let mut stack: Stack = Vec::new();
    eval_script(&mut stack, script_sig, flags, checker)?;
    // Core copies the stack whenever P2SH is set but reads the copy only for a P2SH scriptPubKey;
    // nothing else sees it, so the port copies only then.
    let p2sh = flags.contains(ScriptFlags::P2SH) && is_pay_to_script_hash(script_pubkey);
    let stack_copy = p2sh.then(|| stack.clone());
    eval_script(&mut stack, script_pubkey, flags, checker)?;
    if !stack.last().is_some_and(|top| cast_to_bool(top)) {
        return Err(EvalFalse);
    }

    // Additional validation for spend-to-script-hash transactions.
    if let Some(copy) = stack_copy {
        // The scriptSig must be literals-only.
        if !is_push_only(script_sig) {
            return Err(SigPushOnly);
        }
        // Restore the stack. It cannot be empty here: the P2SH scriptPubKey would have failed on
        // an empty stack. Core asserts; the port fails instead of panicking on input it was given.
        stack = copy;
        let Some(redeem_script) = stack.pop() else {
            // census: unreachable (Core's assert(!stack.empty()))
            return Err(UnknownError);
        };
        eval_script(&mut stack, &redeem_script, flags, checker)?;
        if !stack.last().is_some_and(|top| cast_to_bool(top)) {
            return Err(EvalFalse);
        }
    }

    // CLEANSTACK is checked after the P2SH evaluation: a P2SH spend leaves its inputs on the stack
    // until then.
    if flags.contains(ScriptFlags::CLEANSTACK) {
        // CLEANSTACK without P2SH would allow a switch CLEANSTACK->P2SH+CLEANSTACK, which is not a
        // soft fork.
        assert!(flags.contains(ScriptFlags::P2SH), "SCRIPT_VERIFY_CLEANSTACK requires P2SH");
        if stack.len() != 1 {
            return Err(CleanStack);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::checker::NoSignatureChecker;

    fn eval(script: &[u8], flags: ScriptFlags) -> (Result<(), ScriptError>, Stack) {
        let mut stack = Vec::new();
        let r = eval_script(&mut stack, script, flags, &NoSignatureChecker);
        (r, stack)
    }

    #[test]
    fn cast_to_bool_treats_negative_zero_as_false() {
        assert!(!cast_to_bool(&[]));
        assert!(!cast_to_bool(&[0, 0]));
        assert!(!cast_to_bool(&[0, 0x80]));
        assert!(cast_to_bool(&[0x80, 0]));
        assert!(cast_to_bool(&[0, 1]));
    }

    #[test]
    fn condition_stack_tracks_the_first_false() {
        let mut c = ConditionStack::default();
        assert!(c.is_empty() && c.all_true());
        c.push(true);
        c.push(false);
        c.push(true);
        assert!(!c.all_true());
        c.toggle_top(); // a true above the first false: unobservable
        assert!(!c.all_true());
        c.pop();
        c.toggle_top(); // the first false itself
        assert!(c.all_true());
        c.pop();
        c.pop();
        assert!(c.is_empty());
    }

    #[test]
    fn unexecuted_verif_and_disabled_opcodes_still_fail() {
        let none = ScriptFlags::NONE;
        // 0 IF VERIF ENDIF: VERIF is in the IF..ENDIF range, so it reaches the switch.
        assert_eq!(eval(&[OP_0, OP_IF, OP_VERIF, OP_ENDIF], none).0, Err(ScriptError::BadOpcode));
        // 0 IF MUL ENDIF: disabled before the branch is considered.
        assert_eq!(
            eval(&[OP_0, OP_IF, OP_MUL, OP_ENDIF], none).0,
            Err(ScriptError::DisabledOpcode)
        );
        // 0 IF RESERVED ENDIF: an unexecuted OP_RESERVED is fine.
        assert_eq!(eval(&[OP_0, OP_IF, OP_RESERVED, OP_ENDIF], none).0, Ok(()));
        assert_eq!(eval(&[OP_RESERVED], none).0, Err(ScriptError::BadOpcode));
    }

    #[test]
    fn an_empty_script_does_not_check_the_initial_stack_size() {
        let mut stack = vec![vec![1u8]; MAX_STACK_SIZE + 1];
        assert_eq!(eval_script(&mut stack, &[], ScriptFlags::NONE, &NoSignatureChecker), Ok(()));
        assert_eq!(
            eval_script(&mut stack, &[OP_NOP], ScriptFlags::NONE, &NoSignatureChecker),
            Err(ScriptError::StackSize)
        );
    }
}
