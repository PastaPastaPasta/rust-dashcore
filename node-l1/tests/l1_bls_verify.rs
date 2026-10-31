//! `diff_bls_verify` (BS11a-4 D10) against Dash Core v24's in-process oracle: the node's three
//! verifications agree with Core's on the target's public seeds (written by the
//! `bls_wrapper_seeds` example), on random scripts and on mutated seeds, and the seeds reach
//! each verification's accepting and refusing paths, each on invalid operands under each flag,
//! on kept-element and Exotic operands under each flag, and under the legacy flag on points
//! with no usable element; and they give `VERIFY_AGG` unequal key and hash lists.

#[path = "support/bls_wrapper.rs"]
mod support;

use dash_oracle_sys::ffi::{
    ORACLE_BLS_FLAG_LEGACY, ORACLE_BLS_OP_VERIFY, ORACLE_BLS_OP_VERIFY_AGG,
    ORACLE_BLS_OP_VERIFY_SEC, ORACLE_E_ARG, ORACLE_REJECT,
};
use node_l1::bls_wrapper::{VERIFY_OPS, compare_bls_verify};
use node_l1::target;
use support::Target;

const T: Target = Target {
    name: "diff_bls_verify",
    table: &VERIFY_OPS,
    compare: compare_bls_verify,
};

const VERIFIES: [u8; 3] =
    [ORACLE_BLS_OP_VERIFY, ORACLE_BLS_OP_VERIFY_AGG, ORACLE_BLS_OP_VERIFY_SEC];

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
    support::op_reaches(&hits, ORACLE_BLS_OP_VERIFY, &[ORACLE_REJECT], 0);
    support::op_reaches(&hits, ORACLE_BLS_OP_VERIFY_AGG, &[ORACLE_REJECT, ORACLE_E_ARG], 0);
    support::op_reaches(&hits, ORACLE_BLS_OP_VERIFY_SEC, &[ORACLE_REJECT], 0);
    for op in VERIFIES {
        support::ops_take_kept_and_exotic(&hits, &[op]);
        support::legacy_unusable_reached(&hits, op);
    }
}

/// The seeds give `VERIFY_AGG` a non-empty key list of another length than its hash list, under
/// each flag, which Core refuses (`ORACLE_E_ARG`) when the signature is valid.
#[test]
fn public_seeds_reach_unequal_lists() {
    let hits = support::hits(&T);
    for legacy in [0, ORACLE_BLS_FLAG_LEGACY] {
        assert!(
            hits.iter().any(|h| {
                h.op == ORACLE_BLS_OP_VERIFY_AGG
                    && h.flags & ORACLE_BLS_FLAG_LEGACY == legacy
                    && h.operands.unequal
                    && h.status == ORACLE_E_ARG
            }),
            "no refused unequal lists under flag {legacy}"
        );
    }
}

#[test]
#[ignore = "debugging: EXPLAIN=<random seed or input file>"]
fn explain() {
    support::explain(&T);
}
