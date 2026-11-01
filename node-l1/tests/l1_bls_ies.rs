//! `diff_bls_ies` (BS11b D25) against Dash Core v24's in-process oracle: the node's
//! `DHKeyExchange` and multi-recipient BLS-IES (encryption, container and blob decryption, and
//! the secret key a plaintext holds under each flag) agree with Core's on the target's public
//! seeds (written by the `bls_wrapper_seeds` example), on random scripts and on mutated seeds,
//! and the seeds reach every op's accepting and refusing paths on invalid, kept-element and
//! Exotic operands under both flags.

#[path = "support/bls_wrapper.rs"]
mod support;

use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_INPLACE, ORACLE_BLS_FLAG_LEGACY, ORACLE_BLS_OP_DECODE, ORACLE_BLS_OP_DH,
    ORACLE_BLS_OP_IES_DECRYPT, ORACLE_BLS_OP_IES_DECRYPT_BLOB, ORACLE_BLS_OP_IES_ENCRYPT,
    ORACLE_E_ARG, ORACLE_E_DECODE, ORACLE_E_TRAILING, ORACLE_OK, ORACLE_REJECT,
};
use node_l1::bls_wrapper::{IES_OPS, compare_bls_ies};
use node_l1::target;
use support::Target;

const T: Target = Target {
    name: "diff_bls_ies",
    table: &IES_OPS,
    compare: compare_bls_ies,
};

#[test]
fn registered() {
    assert!(target(T.name).is_some());
}

#[test]
fn public_seeds_agree() {
    support::public_seeds_agree(&T, 16);
}

#[test]
fn random_scripts_agree() {
    support::random_scripts_agree(&T, 300);
}

#[test]
fn mutated_seeds_agree() {
    support::mutated_seeds_agree(&T, 8);
}

#[test]
fn public_seeds_reach_every_op() {
    let hits = support::hits(&T);
    support::op_reaches(&hits, ORACLE_BLS_OP_DH, &[ORACLE_REJECT], 0);
    support::op_reaches(&hits, ORACLE_BLS_OP_DH, &[ORACLE_REJECT], ORACLE_BLS_FLAG_INPLACE);
    support::op_reaches(&hits, ORACLE_BLS_OP_IES_ENCRYPT, &[ORACLE_REJECT, ORACLE_E_ARG], 0);
    for op in [ORACLE_BLS_OP_IES_DECRYPT, ORACLE_BLS_OP_IES_DECRYPT_BLOB] {
        support::op_reaches(&hits, op, &[ORACLE_REJECT, ORACLE_E_DECODE, ORACLE_E_TRAILING], 0);
    }
    // The plaintexts' keys (not the loads' decodes): decoded, valid or not, and refused, under
    // each flag.
    for flags in [0, ORACLE_BLS_FLAG_LEGACY] {
        let decodes: Vec<i32> = hits
            .iter()
            .filter(|h| h.op == ORACLE_BLS_OP_DECODE && h.operands.plaintext && h.flags == flags)
            .map(|h| h.status)
            .collect();
        for status in [ORACLE_OK, ORACLE_REJECT, ORACLE_E_DECODE] {
            assert!(decodes.contains(&status), "DECODE flags {flags}: status {status}");
        }
    }
    support::ops_take_kept_and_exotic(&hits, &[ORACLE_BLS_OP_DH, ORACLE_BLS_OP_IES_ENCRYPT]);
}

#[test]
#[ignore = "debugging: EXPLAIN=<random seed or input file>"]
fn explain() {
    support::explain(&T);
}
