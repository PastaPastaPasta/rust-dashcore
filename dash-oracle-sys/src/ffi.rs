//! Raw declarations of the libdash_oracle C ABI (`src/test/oracle/oracle.h` in the
//! Dash Core pin branches).
//!
//! `build-oracle.sh` renames every `oracle_<name>` entry point of a pin to
//! `oracle_<pin>_<name>` and hides all other symbols, so each pin gets its own
//! module here with identical signatures.

use std::os::raw::{c_char, c_int};

/// `ORACLE_ABI_VERSION` of the header these declarations match.
pub const ABI_VERSION: c_int = 31;

pub const ORACLE_OK: c_int = 0;
pub const ORACLE_REJECT: c_int = 1;
pub const ORACLE_E_DECODE: c_int = -1;
pub const ORACLE_E_EXCEPTION: c_int = -2;
pub const ORACLE_E_BUFFER: c_int = -3;
pub const ORACLE_E_ARG: c_int = -4;
pub const ORACLE_E_NETWORK: c_int = -5;
pub const ORACLE_E_TRAILING: c_int = -6;
pub const ORACLE_E_CONTEXT: c_int = -7;
pub const ORACLE_E_UNSUPPORTED: c_int = -8;
pub const ORACLE_E_POISONED: c_int = -9;

// oracle_chain_step's op tags (ORACLE_CHAIN_*).
pub const ORACLE_CHAIN_BLOCK: u8 = 1;
pub const ORACLE_CHAIN_HEADER: u8 = 2;
pub const ORACLE_CHAIN_PROPOSAL: u8 = 3;
pub const ORACLE_CHAIN_INVALIDATE: u8 = 4;
pub const ORACLE_CHAIN_RECONSIDER: u8 = 5;
pub const ORACLE_CHAIN_PRECIOUS: u8 = 6;
pub const ORACLE_CHAIN_ACTIVATE: u8 = 7;
pub const ORACLE_CHAIN_CL_RECORD: u8 = 8;
pub const ORACLE_CHAIN_CL_ENFORCE: u8 = 9;
pub const ORACLE_CHAIN_SPORK: u8 = 10;
pub const ORACLE_CHAIN_FLUSH: u8 = 11;
pub const ORACLE_CHAIN_RESTART: u8 = 12;
pub const ORACLE_CHAIN_SETTIME: u8 = 13;

/// `oracle_bls_exec` op codes; oracle.h lists each op's operands.
pub const ORACLE_BLS_OP_DECODE: u8 = 0;
pub const ORACLE_BLS_OP_SER: u8 = 1;
pub const ORACLE_BLS_OP_SIGN: u8 = 2;
pub const ORACLE_BLS_OP_PUBKEY: u8 = 3;
pub const ORACLE_BLS_OP_VERIFY: u8 = 4;
pub const ORACLE_BLS_OP_VERIFY_AGG: u8 = 5;
pub const ORACLE_BLS_OP_VERIFY_SEC: u8 = 6;
pub const ORACLE_BLS_OP_AGG_SK: u8 = 7;
pub const ORACLE_BLS_OP_AGG_PK: u8 = 8;
pub const ORACLE_BLS_OP_AGG_SIG: u8 = 9;
pub const ORACLE_BLS_OP_SUB_SIG: u8 = 10;
pub const ORACLE_BLS_OP_AGG_SECURE: u8 = 11;
pub const ORACLE_BLS_OP_SK_SHARE: u8 = 12;
pub const ORACLE_BLS_OP_PK_SHARE: u8 = 13;
pub const ORACLE_BLS_OP_RECOVER_SIG: u8 = 14;
pub const ORACLE_BLS_OP_DH: u8 = 15;
pub const ORACLE_BLS_OP_IES_ENCRYPT: u8 = 16;
pub const ORACLE_BLS_OP_IES_DECRYPT: u8 = 17;
pub const ORACLE_BLS_OP_IES_DECRYPT_BLOB: u8 = 18;
pub const ORACLE_BLS_OP_HASH_TO_G2: u8 = 19;
pub const ORACLE_BLS_OP_EQ: u8 = 20;
pub const ORACLE_BLS_OP_ADD: u8 = 21;
pub const ORACLE_BLS_OP_NEG: u8 = 22;
pub const ORACLE_BLS_OP_MUL: u8 = 23;
pub const ORACLE_BLS_OP_ISVALID: u8 = 24;
pub const ORACLE_BLS_OP_SER_HASH: u8 = 25;
pub const ORACLE_BLS_OP_IMPL_EQ: u8 = 26;
/// `oracle_bls_exec` op flags: the legacy scheme (clear: basic).
pub const ORACLE_BLS_FLAG_LEGACY: u8 = 0x01;
/// Append the op's relic call log to its record (ABI 26).
pub const ORACLE_BLS_FLAG_LOG: u8 = 0x02;
/// Ops 8 and 9: the pair form `AggregateInsecure(o)` (ABI 26).
pub const ORACLE_BLS_FLAG_PAIR: u8 = 0x04;
/// Ops 12 to 15: run Core's call on a copy of a receiver register (ABI 26).
pub const ORACLE_BLS_FLAG_INPLACE: u8 = 0x08;

pub const ORACLE_MNLIST_CANONICAL: c_int = 0;
pub const ORACLE_MNLIST_DISK: c_int = 1;

pub const ORACLE_UNIQUE_COLLATERAL: c_int = 0;
pub const ORACLE_UNIQUE_SERVICE: c_int = 1;
pub const ORACLE_UNIQUE_DOMAIN: c_int = 2;
pub const ORACLE_UNIQUE_KEY_ID: c_int = 3;
pub const ORACLE_UNIQUE_PLATFORM_NODE_ID: c_int = 4;
pub const ORACLE_UNIQUE_OPERATOR_KEY: c_int = 5;
pub const ORACLE_MNLIST_OP_ADD: u8 = 0;
pub const ORACLE_MNLIST_OP_UPDATE: u8 = 1;
pub const ORACLE_MNLIST_OP_REMOVE: u8 = 2;
pub const ORACLE_DML_STAGE_NONE: c_int = 0;
pub const ORACLE_DML_STAGE_CHECK: c_int = 1;
pub const ORACLE_DML_STAGE_BUILD: c_int = 2;
pub const ORACLE_DML_STAGE_PROCESS: c_int = 3;
pub const ORACLE_NETINFO_MN: c_int = 0;
pub const ORACLE_NETINFO_EXT: c_int = 1;

/// The name `script_tests.json` uses for each `ScriptError` value, indexed by the value
/// (`ScriptError_t` in `src/script/script_error.h`, identical in both pins; the names are
/// those of Core's `script_tests.cpp` table, plus `IMPOSSIBLE_ENCODING`). Generated from
/// the two files; `SCRIPT_ERR_ERROR_COUNT` is the length.
pub const SCRIPT_ERROR_NAMES: [&str; 41] = [
    "OK",
    "UNKNOWN_ERROR",
    "EVAL_FALSE",
    "OP_RETURN",
    "SCRIPT_SIZE",
    "PUSH_SIZE",
    "OP_COUNT",
    "STACK_SIZE",
    "SIG_COUNT",
    "PUBKEY_COUNT",
    "VERIFY",
    "EQUALVERIFY",
    "CHECKMULTISIGVERIFY",
    "CHECKSIGVERIFY",
    "CHECKDATASIGVERIFY",
    "NUMEQUALVERIFY",
    "BAD_OPCODE",
    "DISABLED_OPCODE",
    "INVALID_STACK_OPERATION",
    "INVALID_ALTSTACK_OPERATION",
    "UNBALANCED_CONDITIONAL",
    "SPLIT_RANGE",
    "OPERAND_SIZE",
    "INVALID_NUMBER_RANGE",
    "IMPOSSIBLE_ENCODING",
    "DIV_BY_ZERO",
    "MOD_BY_ZERO",
    "NEGATIVE_LOCKTIME",
    "UNSATISFIED_LOCKTIME",
    "SIG_HASHTYPE",
    "SIG_DER",
    "MINIMALDATA",
    "SIG_PUSHONLY",
    "SIG_HIGH_S",
    "SIG_NULLDUMMY",
    "PUBKEYTYPE",
    "CLEANSTACK",
    "NULLFAIL",
    "DISCOURAGE_UPGRADABLE_NOPS",
    "OP_CODESEPARATOR",
    "SIG_FINDANDDELETE",
];

/// `oracle_validation`: the reject details of a `CValidationState`.
#[repr(C)]
pub struct OracleValidation {
    pub result: c_int,
    pub reject_reason: [c_char; 128],
    pub debug_message: [c_char; 512],
    /// Full length of the reason (ABI 6): above 127 the buffer holds a prefix.
    pub reject_reason_len: u32,
    /// Full length of the debug message (ABI 6): above 511 the buffer holds a prefix.
    pub debug_message_len: u32,
}

impl Default for OracleValidation {
    fn default() -> Self {
        Self {
            result: 0,
            reject_reason: [0; 128],
            debug_message: [0; 512],
            reject_reason_len: 0,
            debug_message_len: 0,
        }
    }
}

/// `ORACLE_P2P_FORMAT`: [`OracleP2pCtx::format`] of this ABI.
pub const ORACLE_P2P_FORMAT: u32 = 1;

/// `oracle_p2p_ctx`: the stream a P2P message is read from (ABI 7).
#[repr(C)]
pub struct OracleP2pCtx {
    pub format: u32,
    pub stream_version: i32,
    pub bls_legacy: i32,
    pub qdata_max_vvec: u32,
    pub qdata_max_contrib: u32,
}

/// `oracle_p2p_fields`: the qdata handler's view, -1 where it does not apply (ABI 7).
#[repr(C)]
pub struct OracleP2pFields {
    pub qdata_error: i32,
    pub qdata_vvec_size: i64,
    pub qdata_contrib_count: i64,
    pub reject_reason: [c_char; 64],
}

impl Default for OracleP2pFields {
    fn default() -> Self {
        Self {
            qdata_error: -1,
            qdata_vvec_size: -1,
            qdata_contrib_count: -1,
            reject_reason: [0; 64],
        }
    }
}

/// `oracle_addrman`: an opaque Core `AddrMan` handle (ABI 10).
#[repr(C)]
pub struct OracleAddrman {
    _private: [u8; 0],
}

/// `oracle_peer_session`: an opaque node with test peers (ABI 10).
#[repr(C)]
pub struct OraclePeerSession {
    _private: [u8; 0],
}

/// `oracle_llmq_ctx`: Core's quorum managers over a caller-seeded chain (ABI 11).
#[repr(C)]
pub struct OracleLlmqCtx {
    _private: [u8; 0],
}

/// `oracle_chain`: an opaque chain session (ABI 22).
#[repr(C)]
pub struct OracleChain {
    _private: [u8; 0],
}

/// `oracle_chain_image`: an opaque copy of a chain session's storage (ABI 22).
#[repr(C)]
pub struct OracleChainImage {
    _private: [u8; 0],
}
/// `oracle_lock_verdict`: a lock or recovered-signature verdict (ABI 26).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OracleLockVerdict {
    /// `ORACLE_LOCK_*`.
    pub status: i32,
    /// `ORACLE_PASS_*` of each pass.
    pub pass_status: [i32; 2],
    /// Each pass's quorum hash; zero without a quorum.
    pub pass_quorum: [[u8; 32]; 2],
    /// `::SerializeHash` of the message as Core read it: its inv hash.
    pub hash: [u8; 32],
}

// oracle.h's layout, which is the ABI: no padding.
const _: () = assert!(std::mem::size_of::<OracleLockVerdict>() == 108);

/// `oracle_substitution`: a capture-and-substitute marker of `peer_session_deliver`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OracleSubstitution {
    pub dst_offset: u32,
    pub src_peer: i64,
    /// NUL-terminated.
    pub src_command: [c_char; 13],
    pub src_offset: u32,
    pub len: u32,
}

/// Calls `$callback!(($($extra)*) <every entry point>)`, so the signatures are written once.
macro_rules! oracle_fns {
    ($callback:ident!($($extra:tt)*)) => {
        $callback! { ($($extra)*)
            fn abi_version() -> c_int;
            fn core_version() -> *const c_char;
            fn last_error(out: *mut c_char, cap: usize) -> usize;
            fn script_error_string(err: c_int, out: *mut c_char, cap: usize) -> usize;
            fn set_network(chain: *const c_char, args: *const *const c_char, n_args: usize) -> c_int;
            fn chainparams_json(out: *mut c_char, len: *mut usize) -> c_int;
            fn tx_decode(input: *const u8, in_len: usize, consumed: *mut usize, reenc: *mut u8,
                         reenc_len: *mut usize, txid: *mut u8, instance_hash: *mut u8) -> c_int;
            fn block_decode(input: *const u8, in_len: usize, consumed: *mut usize, reenc: *mut u8,
                            reenc_len: *mut usize, hash: *mut u8, merkle_root: *mut u8, mutated: *mut c_int) -> c_int;
            fn header_decode(input: *const u8, in_len: usize, consumed: *mut usize, reenc: *mut u8,
                             hash: *mut u8) -> c_int;
            fn merkle_root(leaves: *const u8, n: usize, root: *mut u8, mutated: *mut c_int) -> c_int;
            fn coin_codec(op: c_int, input: *const u8, in_len: usize, consumed: *mut usize, out: *mut u8,
                          out_len: *mut usize) -> c_int;
            fn check_transaction(tx: *const u8, tx_len: usize, state: *mut OracleValidation) -> c_int;
            fn check_block(block: *const u8, block_len: usize, check_pow: c_int, check_merkle_root: c_int,
                           state: *mut OracleValidation) -> c_int;
            fn legacy_sigops(tx: *const u8, tx_len: usize, count: *mut u32) -> c_int;
            fn verify_script(spk: *const u8, spk_len: usize, tx: *const u8, tx_len: usize, n_in: u32,
                             amount: i64, flags: u32, script_err: *mut c_int) -> c_int;
            fn signature_hash(script: *const u8, script_len: usize, tx: *const u8, tx_len: usize, n_in: u32,
                              hash_type: i32, out: *mut u8) -> c_int;
            fn next_work_required(headers: *const u8, n: usize, first_height: i32, new_header: *const u8,
                                  bits: *mut u32) -> c_int;
            fn check_proof_of_work(hash: *const u8, bits: u32) -> c_int;
            fn compact_decode(bits: u32, target: *mut u8, negative: *mut c_int, overflow: *mut c_int) -> c_int;
            fn compact_encode(target: *const u8, bits: *mut u32) -> c_int;
            fn block_proof(bits: u32, proof: *mut u8) -> c_int;
            fn block_subsidy_inner(prev_bits: u32, prev_height: i32, v20: c_int, out: *mut i64) -> c_int;
            fn superblock_subsidy_inner(prev_bits: u32, prev_height: i32, v20: c_int, out: *mut i64) -> c_int;
            fn mn_payment(height: i32, block_value: i64, era: c_int, out: *mut i64) -> c_int;
            fn platform_share(mn_reward: i64, out: *mut i64) -> c_int;
            fn eval_script(script: *const u8, script_len: usize, stack_in: *const u8, stack_in_len: usize, flags: u32,
                           stack_out: *mut u8, stack_out_len: *mut usize, script_err: *mut c_int) -> c_int;
            fn pubkey_verify(pubkey: *const u8, pubkey_len: usize, msg: *const u8, sig: *const u8, sig_len: usize) -> c_int;
            fn check_low_s(sig: *const u8, sig_len: usize) -> c_int;
            fn parse_der_lax(sig: *const u8, sig_len: usize, compact: *mut u8) -> c_int;
            fn check_signature_encoding(sig: *const u8, sig_len: usize, flags: u32, script_err: *mut c_int) -> c_int;
            fn check_minimal_push(data: *const u8, data_len: usize, opcode: c_int) -> c_int;
            fn scriptnum_decode(vch: *const u8, vch_len: usize, require_minimal: c_int, max_size: usize, value: *mut i64,
                                value_int: *mut i32) -> c_int;
            fn scriptnum_encode(value: i64, out: *mut u8, out_len: *mut usize) -> c_int;
            fn scriptnum_is_minimally_encoded(vch: *const u8, vch_len: usize, max_size: usize) -> c_int;
            fn scriptnum_minimally_encode(vch: *const u8, vch_len: usize, out: *mut u8, out_len: *mut usize,
                                          changed: *mut c_int) -> c_int;
            fn find_and_delete(script: *const u8, script_len: usize, pattern: *const u8, pattern_len: usize, out: *mut u8,
                               out_len: *mut usize, count: *mut c_int) -> c_int;
            fn script_sigops(script: *const u8, script_len: usize, accurate: c_int, count: *mut u32) -> c_int;
            fn script_sigops_with_sig(spk: *const u8, spk_len: usize, script_sig: *const u8, script_sig_len: usize,
                                      count: *mut u32) -> c_int;
            fn transaction_sigops(tx: *const u8, tx_len: usize, spent_outputs: *const u8, spent_outputs_len: usize,
                                  flags: u32, legacy: *mut u32, p2sh: *mut u32, total: *mut u32) -> c_int;
            fn check_input_scripts(tx: *const u8, tx_len: usize, spent_outputs: *const u8, spent_outputs_len: usize,
                                   flags: u32, state: *mut OracleValidation) -> c_int;
            fn convert_bits_to_double(bits: u32, out: *mut f64) -> c_int;
            fn superblock_payments_limit(height: i32, out: *mut i64) -> c_int;
            fn split_amount_by_shares(total: i64, weights: *const i64, n: usize, amounts: *mut i64) -> c_int;
            fn check_special_tx(tx: *const u8, tx_len: usize, ctx: *const u8, ctx_len: usize, check_sigs: c_int,
                                state: *mut OracleValidation) -> c_int;
            fn dml_apply(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize, new_list: *mut u8,
                         new_list_len: *mut usize, canonical: *mut u8, canonical_len: *mut usize, payee: *mut u8,
                         has_payee: *mut c_int, state: *mut OracleValidation) -> c_int;
            fn dml_listdiff_json(from_list: *const u8, from_len: usize, to_list: *const u8, to_len: usize,
                                 out: *mut c_char, len: *mut usize) -> c_int;
            fn check_tx_inputs(tx: *const u8, tx_len: usize, coins: *const u8, coins_len: usize, spend_height: i32,
                               fee: *mut i64, state: *mut OracleValidation) -> c_int;
            fn sequence_locks(tx: *const u8, tx_len: usize, ctx: *const u8, ctx_len: usize, prev_heights: *const i32,
                              n: usize, flags: i32, min_height: *mut i32, min_time: *mut i64,
                              satisfied: *mut c_int) -> c_int;
            fn contextual_check_tx(tx: *const u8, tx_len: usize, ctx: *const u8, ctx_len: usize,
                                   state: *mut OracleValidation) -> c_int;
            fn contextual_check_header(header: *const u8, ctx: *const u8, ctx_len: usize,
                                       state: *mut OracleValidation) -> c_int;
            fn contextual_check_block(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize,
                                      state: *mut OracleValidation) -> c_int;
            fn versionbits_state(ctx: *const u8, ctx_len: usize, signals: *const u8, signals_len: usize,
                                 queries: *const u8, queries_len: usize, out: *mut u8,
                                 out_len: *mut usize) -> c_int;
            fn verify_commitment(qc: *const u8, qc_len: usize, ctx: *const u8, ctx_len: usize, check_sigs: c_int,
                                 state: *mut OracleValidation) -> c_int;
            fn check_special_tx_block(tx: *const u8, tx_len: usize, ctx: *const u8, ctx_len: usize,
                                      block_ctx: *const u8, block_ctx_len: usize, check_sigs: c_int,
                                      state: *mut OracleValidation) -> c_int;
            fn netinfo_validate(kind: c_int, netinfo: *const u8, netinfo_len: usize, status: *mut u8) -> c_int;
            fn netinfo_add_entry(kind: c_int, netinfo: *const u8, netinfo_len: usize, purpose: u8, input: *const u8,
                                 input_len: usize, status: *mut u8, entry: *mut u8, entry_len: *mut usize,
                                 netinfo_out: *mut u8, netinfo_out_len: *mut usize) -> c_int;
            fn bls_decode(kind: c_int, mode: c_int, legacy: c_int, input: *const u8, in_len: usize, valid: *mut c_int,
                          reser: *mut u8, reser_len: *mut usize) -> c_int;
            fn bls_verify(legacy: c_int, pubkey: *const u8, sig: *const u8, msg: *const u8) -> c_int;
            fn bls_exec(script: *const u8, script_len: usize, out: *mut u8, out_len: *mut usize) -> c_int;
            fn test_bls_inject(symbol: *const c_char, nth: u32, code: i32, anywhere: c_int) -> c_int;
            fn test_bls_set_code(code: i32) -> c_int;
            fn payload_decode(tx_type: u16, bls_legacy: c_int, payload: *const u8, payload_len: usize,
                              consumed_all: *mut c_int, reenc: *mut u8, reenc_len: *mut usize) -> c_int;
            fn p2p_decode(command: *const c_char, payload: *const u8, payload_len: usize, ctx: *const OracleP2pCtx,
                          consumed: *mut usize, reenc: *mut u8, reenc_len: *mut usize,
                          fields: *mut OracleP2pFields) -> c_int;
            fn mnlist_canonical(input: c_int, list: *const u8, list_len: usize, canonical: *mut u8,
                                canonical_len: *mut usize, disk: *mut u8, disk_len: *mut usize,
                                hash: *mut u8) -> c_int;
            fn unique_property_hash(kind: c_int, bls_legacy: c_int, value: *const u8, value_len: usize,
                                    preimage: *mut u8, preimage_len: *mut usize, hash: *mut u8) -> c_int;
            fn state_diff(a: *const u8, a_len: usize, b: *const u8, b_len: usize, diff: *mut u8,
                          diff_len: *mut usize, applied: *mut u8, applied_len: *mut usize) -> c_int;
            fn mnlist_diff(input: c_int, from_list: *const u8, from_len: usize, to_list: *const u8,
                           to_len: usize, canonical: *mut u8, canonical_len: *mut usize, disk: *mut u8,
                           disk_len: *mut usize) -> c_int;
            fn mnlist_apply_diff(list_input: c_int, list: *const u8, list_len: usize, diff_input: c_int,
                                 diff: *const u8, diff_len: usize, block_hash: *const u8, height: i32,
                                 new_list: *mut u8, new_list_len: *mut usize, hash: *mut u8) -> c_int;
            fn mnlist_core_order(ops: *const u8, ops_len: usize, order: *mut u8, order_len: *mut usize,
                                 canonical: *mut u8, canonical_len: *mut usize, failed_op: *mut i64) -> c_int;
            fn mnlist_payee(input: c_int, list: *const u8, list_len: usize, prev_height: i32, payee: *mut u8,
                            has_payee: *mut c_int) -> c_int;
            fn mnlist_projected(input: c_int, list: *const u8, list_len: usize, prev_height: i32, count: i32,
                                payees: *mut u8, payees_len: *mut usize) -> c_int;
            fn dml_block(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize, check_sigs: c_int,
                         just_check: c_int, stage: *mut c_int, canonical: *mut u8, canonical_len: *mut usize,
                         hash: *mut u8, payee: *mut u8, has_payee: *mut c_int,
                         state: *mut OracleValidation) -> c_int;
            fn dml_block_effects(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize,
                                 check_sigs: c_int, just_check: c_int, stage: *mut c_int, canonical: *mut u8,
                                 canonical_len: *mut usize, hash: *mut u8, payee: *mut u8, has_payee: *mut c_int,
                                 state: *mut OracleValidation, effects: *mut u8,
                                 effects_len: *mut usize) -> c_int;
            fn sml_entry_hash(dmn: *const u8, dmn_len: usize, entry: *mut u8, entry_len: *mut usize,
                              hash: *mut u8) -> c_int;
            fn sml_merkle_root(input: c_int, list: *const u8, list_len: usize, hashes: *mut u8,
                               hashes_len: *mut usize, root: *mut u8, state: *mut OracleValidation) -> c_int;
            fn special_tx_sign_hash(tx: *const u8, tx_len: usize, payload_hash: *mut u8, sign_string: *mut c_char,
                                    sign_string_len: *mut usize, message_hash: *mut u8, consent_hash: *mut u8,
                                    has_consent: *mut c_int) -> c_int;

            fn addrman_new(deterministic: c_int, rng_seed: u64, asmap: *const u8, asmap_len: usize, check_ratio: i32,
                           out: *mut *mut OracleAddrman) -> c_int;
            fn addrman_free(addrman: *mut OracleAddrman) -> ();
            fn addrman_op(addrman: *mut OracleAddrman, op: *const u8, op_len: usize, mocktime: i64, out: *mut u8,
                          out_len: *mut usize) -> c_int;
            fn addrman_canonical(addrman: *mut OracleAddrman, out: *mut u8, out_len: *mut usize) -> c_int;
            fn addrman_bucket(nkey: *const u8, addr: *const u8, addr_len: usize, src: *const u8, src_len: usize,
                              asmap: *const u8, asmap_len: usize, tried_bucket: *mut u32, new_bucket: *mut u32,
                              pos_new: *mut u32, pos_tried: *mut u32) -> c_int;
            fn addrman_chance(last_try_delta_s: i64, attempts: i32, double_bits: *mut u64) -> c_int;
            fn netgroup(addr: *const u8, addr_len: usize, asmap: *const u8, asmap_len: usize, out: *mut u8,
                        out_len: *mut usize, asn: *mut u32) -> c_int;
            fn peer_session_new(chain: *const c_char, args: *const *const c_char, n_args: usize, our_services: u64,
                                mocktime: i64, rng_seed: u64, chain_fixture: *const u8, chain_fixture_len: usize,
                                out: *mut *mut OraclePeerSession) -> c_int;
            fn peer_session_add_peer(session: *mut OraclePeerSession, conn_type: c_int, transport: c_int,
                                     send_mode: c_int, addr: *const u8, addr_len: usize, peer: *mut i64) -> c_int;
            fn peer_session_deliver(session: *mut OraclePeerSession, peer: i64, command: *const c_char,
                                    payload: *const u8, payload_len: usize, subs: *const OracleSubstitution,
                                    n_subs: usize) -> c_int;
            fn peer_session_drain(session: *mut OraclePeerSession, peer: i64, max_bytes: u64,
                                  written: *mut u64) -> c_int;
            fn peer_session_send_pass(session: *mut OraclePeerSession, mocktime: i64) -> c_int;
            fn peer_session_tick(session: *mut OraclePeerSession, mocktime: i64, what: u32) -> c_int;
            fn peer_session_outcome(session: *mut OraclePeerSession, peer: i64, out: *mut u8,
                                    out_len: *mut usize) -> c_int;
            fn peer_session_free(session: *mut OraclePeerSession) -> ();
            fn llmq_ctx_new(chain: *const c_char, args: *const *const c_char, n_args: usize,
                            out: *mut *mut OracleLlmqCtx) -> c_int;
            fn llmq_ctx_free(ctx: *mut OracleLlmqCtx) -> ();
            fn llmq_add_header(ctx: *mut OracleLlmqCtx, header: *const u8, height: i32) -> c_int;
            fn llmq_set_mn_list(ctx: *mut OracleLlmqCtx, block: *const u8, list: *const u8, len: usize) -> c_int;
            fn llmq_set_coinbase(ctx: *mut OracleLlmqCtx, block: *const u8, tx: *const u8, len: usize,
                                 have_data: c_int) -> c_int;
            fn llmq_set_snapshot(ctx: *mut OracleLlmqCtx, llmq_type: u8, cycle_base: *const u8, snapshot: *const u8,
                                 len: usize) -> c_int;
            fn llmq_set_mined(ctx: *mut OracleLlmqCtx, llmq_type: u8, commitment: *const u8, len: usize,
                              mined_height: i32) -> c_int;
            fn llmq_ctx_load(ctx: *mut OracleLlmqCtx, fixture: *const u8, len: usize) -> c_int;
            fn llmq_type_enabled(ctx: *mut OracleLlmqCtx, llmq_type: u8, prev: *const u8, opt_dip24_active: c_int,
                                 opt_have_dip24_quorums: c_int, enabled: *mut c_int) -> c_int;
            fn llmq_members(ctx: *mut OracleLlmqCtx, llmq_type: u8, base: *const u8, legacy_bls: c_int,
                            reset_cache: c_int, members: *mut u8, members_len: *mut usize, trace: *mut u8,
                            trace_len: *mut usize) -> c_int;
            fn llmq_precompute(ctx: *mut OracleLlmqCtx, block: *const u8, legacy_bls: c_int, reset_cache: c_int,
                               trace: *mut u8, trace_len: *mut usize) -> c_int;
            fn llmq_scan(ctx: *mut OracleLlmqCtx, llmq_type: u8, start: *const u8, count: u32,
                         use_chain_overload: c_int, bases: *mut u8, bases_len: *mut usize, trace: *mut u8,
                         trace_len: *mut usize) -> c_int;
            fn llmq_select(ctx: *mut OracleLlmqCtx, llmq_type: u8, selection_hash: *const u8, sign_height: i32,
                           offset: i32, out: *mut u8, found: *mut c_int, trace: *mut u8,
                           trace_len: *mut usize) -> c_int;
            fn bls_legacy_scheme(legacy: *mut c_int) -> c_int;
            fn llmq_restart(ctx: *mut OracleLlmqCtx) -> c_int;
            fn llmq_rows_digest(ctx: *mut OracleLlmqCtx, out: *mut u8) -> c_int;
            fn llmq_verify_chainlock(ctx: *mut OracleLlmqCtx, clsig: *const u8, len: usize, legacy_bls: c_int,
                                     verdict: *mut OracleLockVerdict, trace: *mut u8,
                                     trace_len: *mut usize) -> c_int;
            fn llmq_verify_islock(ctx: *mut OracleLlmqCtx, isdlock: *const u8, len: usize, legacy_bls: c_int,
                                  verdict: *mut OracleLockVerdict, trace: *mut u8,
                                  trace_len: *mut usize) -> c_int;
            fn llmq_verify_qsigrec(ctx: *mut OracleLlmqCtx, qsigrec: *const u8, len: usize, legacy_bls: c_int,
                                   verdict: *mut OracleLockVerdict, trace: *mut u8,
                                   trace_len: *mut usize) -> c_int;
            fn llmq_verify_mined(ctx: *mut OracleLlmqCtx, qc: *const u8, len: usize, legacy_bls: c_int,
                                 ok: *mut c_int, stage: *mut c_int, trace: *mut u8,
                                 trace_len: *mut usize) -> c_int;
            fn coins_ops(script: *const u8, script_len: usize, db_batch_size: i64, out: *mut u8,
                         out_len: *mut usize) -> c_int;
            fn block_value_payee(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize,
                                 pay_ctx: *const u8, pay_ctx_len: usize, out: *mut u8, out_len: *mut usize,
                                 state: *mut OracleValidation) -> c_int;
            fn credit_pool_step(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize,
                                step_ctx: *const u8, step_ctx_len: usize, out: *mut u8, out_len: *mut usize,
                                state: *mut OracleValidation) -> c_int;
            fn chain_new(args: *const *const c_char, n_args: usize, mocktime: i64, image: *const OracleChainImage,
                         out: *mut *mut OracleChain, out_err: *mut c_char, out_err_len: *mut usize) -> c_int;
            fn chain_step(s: *mut OracleChain, op: *const u8, op_len: usize, out: *mut u8,
                          out_len: *mut usize) -> c_int;
            fn chain_report_bound(s: *const OracleChain) -> usize;
            fn chain_save(s: *mut OracleChain, out: *mut *mut OracleChainImage) -> c_int;
            fn chain_restore(s: *mut OracleChain, img: *const OracleChainImage, out: *mut u8,
                             out_len: *mut usize) -> c_int;
            fn chain_image_free(img: *mut OracleChainImage) -> ();
            fn chain_free(s: *mut OracleChain) -> c_int;
            fn cj_rule(op: c_int, args: *const i64, n: usize, out: *mut i64) -> c_int;
            fn cj_entry(kind: c_int, vin: *const u8, vin_len: usize, vout: *const u8, vout_len: usize,
                        session_denom: i32, message: *mut i32) -> c_int;
            fn cj_in_outs(view: *const u8, view_len: usize, vin: *const u8, vin_len: usize, vout: *const u8,
                          vout_len: usize, session_denom: i32, allow_rebalance_shapes: c_int, final_tx: c_int,
                          message: *mut i32, consume_collateral: *mut c_int, counts: *mut u64) -> c_int;
            fn cj_collateral(view: *const u8, view_len: usize, tx: *const u8, tx_len: usize, accept: c_int,
                             base_fees: i64, calls: *mut u32) -> c_int;
            fn cj_message_hash(kind: c_int, msg: *const u8, msg_len: usize, sig_hash: *mut u8,
                               hash: *mut u8) -> c_int;
            fn cj_check_signature(kind: c_int, msg: *const u8, msg_len: usize, pubkey: *const u8,
                                  pubkey_legacy: c_int) -> c_int;
            fn cj_queue_time_out_of_bounds(dsq: *const u8, dsq_len: usize, current_time: i64,
                                           out_of_bounds: *mut c_int) -> c_int;
            fn cj_dstx_structure(dstx: *const u8, dstx_len: usize, ctx: *const u8, ctx_len: usize,
                                 possibly_valid_post_v24: *mut c_int) -> c_int;
            fn mnhf_step(block: *const u8, block_len: usize, ctx: *const u8, ctx_len: usize,
                         mnhf_ctx: *const u8, mnhf_ctx_len: usize, out: *mut u8, out_len: *mut usize,
                         state: *mut OracleValidation) -> c_int;
            fn gov_proposal_validate(data_hex: *const c_char, data_hex_len: usize, check_expiration: c_int,
                                     allow_script: c_int, adjusted_time: i64, valid: *mut c_int,
                                     errors: *mut c_char, errors_len: *mut usize) -> c_int;
            fn gov_object(obj: *const u8, obj_len: usize, ctx: *const u8, ctx_len: usize, gov_ctx: *const u8,
                          gov_ctx_len: usize, out: *mut u8, out_len: *mut usize) -> c_int;
            fn gov_vote(vote: *const u8, vote_len: usize, ctx: *const u8, ctx_len: usize, out: *mut u8,
                        out_len: *mut usize) -> c_int;
            fn gov_ops(ctx: *const u8, ctx_len: usize, gov_ctx: *const u8, gov_ctx_len: usize, ops: *const u8,
                       ops_len: usize, out: *mut u8, out_len: *mut usize) -> c_int;
            fn mnsync_step(start_time: i64, ops: *const u8, ops_len: usize, out: *mut u8,
                           out_len: *mut usize) -> c_int;
            fn netfulfilled_step(start_time: i64, ops: *const u8, ops_len: usize, out: *mut u8,
                                 out_len: *mut usize) -> c_int;
            fn mncache_step(ops: *const u8, ops_len: usize, out: *mut u8, out_len: *mut usize) -> c_int;
            fn flatdb_load(kind: c_int, mocktime: i64, file: *const u8, file_len: usize, present: c_int,
                           load_cache: c_int, out: *mut u8, out_len: *mut usize) -> c_int;
        }
    };
}

macro_rules! api_struct {
    (() $(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;)*) => {
        /// The entry points of one pin.
        #[derive(Clone, Copy, Debug)]
        pub struct Api {
            $(pub $name: unsafe extern "C" fn($($ty),*) -> $ret,)*
        }
    };
}

macro_rules! pin_module {
    (($module:ident, $prefix:literal) $(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;)*) => {
        #[doc = concat!("Entry points `", $prefix, "*`.")]
        pub mod $module {
            use super::*;

            unsafe extern "C" {
                $(
                    #[link_name = concat!($prefix, stringify!($name))]
                    pub fn $name($($arg: $ty),*) -> $ret;
                )*
            }

            pub static API: Api = Api { $($name,)* };
        }
    };
}

oracle_fns!(api_struct!());
oracle_fns!(pin_module!(v24_0, "oracle_v24_0_"));
