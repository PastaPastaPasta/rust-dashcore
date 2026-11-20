//! `diff_bls_aggregate` (BS11b D25) against Dash Core v24's in-process oracle: the node's
//! aggregates, `SubInsecure` and secure aggregate agree with Core's on the target's public
//! seeds (written by the `bls_wrapper_seeds` example), on random scripts and on mutated seeds,
//! and the seeds reach every op's accepting and refusing paths on invalid, kept-element and
//! Exotic operands under both flags.

#[path = "support/bls_wrapper.rs"]
mod support;

use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_LEGACY, ORACLE_BLS_FLAG_PAIR, ORACLE_BLS_OP_AGG_PK, ORACLE_BLS_OP_AGG_SECURE,
    ORACLE_BLS_OP_AGG_SIG, ORACLE_BLS_OP_AGG_SK, ORACLE_BLS_OP_SER_HASH, ORACLE_BLS_OP_SUB_SIG,
    ORACLE_BLS_OP_VERIFY_SEC, ORACLE_E_ARG, ORACLE_REJECT,
};
use node_l1::bls_wrapper::{AGGREGATE_OPS, compare_bls_aggregate};
use node_l1::target;
use support::Target;

const T: Target = Target {
    name: "diff_bls_aggregate",
    table: &AGGREGATE_OPS,
    compare: compare_bls_aggregate,
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
    support::op_reaches(&hits, ORACLE_BLS_OP_AGG_SK, &[ORACLE_REJECT], 0);
    for op in [ORACLE_BLS_OP_AGG_PK, ORACLE_BLS_OP_AGG_SIG] {
        support::op_reaches(&hits, op, &[ORACLE_REJECT], 0);
        support::op_reaches(&hits, op, &[ORACLE_E_ARG], ORACLE_BLS_FLAG_PAIR);
    }
    support::op_reaches(&hits, ORACLE_BLS_OP_SUB_SIG, &[ORACLE_E_ARG], 0);
    support::op_reaches(&hits, ORACLE_BLS_OP_AGG_SECURE, &[ORACLE_REJECT], 0);
    support::op_reaches(&hits, ORACLE_BLS_OP_VERIFY_SEC, &[ORACLE_REJECT], 0);
    support::legacy_unusable_reached(&hits, ORACLE_BLS_OP_VERIFY_SEC);
    // `GetHash()` of an invalid object, under each flag.
    for flags in [0, ORACLE_BLS_FLAG_LEGACY] {
        assert!(hits.iter().any(|h| {
            h.op == ORACLE_BLS_OP_SER_HASH && h.status == ORACLE_REJECT && h.flags == flags
        }));
    }
    support::ops_take_kept_and_exotic(
        &hits,
        &[
            ORACLE_BLS_OP_AGG_SK,
            ORACLE_BLS_OP_AGG_PK,
            ORACLE_BLS_OP_AGG_SIG,
            ORACLE_BLS_OP_SUB_SIG,
            ORACLE_BLS_OP_AGG_SECURE,
            ORACLE_BLS_OP_VERIFY_SEC,
        ],
    );
}

#[test]
#[ignore = "debugging: EXPLAIN=<random seed or input file>"]
fn explain() {
    support::explain(&T);
}
