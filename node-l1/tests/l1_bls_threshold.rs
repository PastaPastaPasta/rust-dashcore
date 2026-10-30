//! `diff_bls_threshold` (BS11b D25) against Dash Core v24's in-process oracle: the node's
//! threshold shares and signature recovery, with and without Core's in-place receiver, and
//! the verification of what they make, agree with Core's on the target's public seeds
//! (written by the `bls_wrapper_seeds` example), on random scripts and on mutated seeds, and
//! the seeds reach every op's accepting and refusing paths on invalid, kept-element and Exotic
//! operands under both flags.

#[path = "support/bls_wrapper.rs"]
mod support;

use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_INPLACE, ORACLE_BLS_OP_PK_SHARE, ORACLE_BLS_OP_RECOVER_SIG,
    ORACLE_BLS_OP_SK_SHARE, ORACLE_BLS_OP_VERIFY, ORACLE_REJECT,
};
use node_l1::bls_wrapper::{THRESHOLD_OPS, compare_bls_threshold};
use node_l1::target;
use support::Target;

const T: Target = Target {
    name: "diff_bls_threshold",
    table: &THRESHOLD_OPS,
    compare: compare_bls_threshold,
};

#[test]
fn registered() {
    assert!(target(T.name).is_some());
}

#[test]
fn public_seeds_agree() {
    support::public_seeds_agree(&T, 20);
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
    for op in [ORACLE_BLS_OP_SK_SHARE, ORACLE_BLS_OP_PK_SHARE, ORACLE_BLS_OP_RECOVER_SIG] {
        support::op_reaches(&hits, op, &[ORACLE_REJECT], 0);
        support::op_reaches(&hits, op, &[ORACLE_REJECT], ORACLE_BLS_FLAG_INPLACE);
    }
    support::op_reaches(&hits, ORACLE_BLS_OP_VERIFY, &[ORACLE_REJECT], 0);
    support::legacy_unusable_reached(&hits, ORACLE_BLS_OP_VERIFY);
    support::ops_take_kept_and_exotic(
        &hits,
        &[
            ORACLE_BLS_OP_SK_SHARE,
            ORACLE_BLS_OP_PK_SHARE,
            ORACLE_BLS_OP_RECOVER_SIG,
            ORACLE_BLS_OP_VERIFY,
        ],
    );
}

#[test]
#[ignore = "debugging: EXPLAIN=<random seed or input file>"]
fn explain() {
    support::explain(&T);
}
