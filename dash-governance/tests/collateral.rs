//! L0: `IsCollateralValid`'s confirmation window and its unmined branch
//! (`governance/object.cpp:815-819`, `:853-873`) on the fee transactions of the Core-produced
//! regtest network (`common`). The oracle cannot take an unmined collateral (it refuses a null
//! block hash, O2a-GOV D4), so the texts are Core's, written out.

mod common;

use common::fixture;
use dash_consensus::decode_tx;
use dash_governance::object::{
    self, FEE_CONFIRMATIONS, FoundCollateral, MIN_RELAY_FEE_CONFIRMATIONS, PROPOSAL,
    collateral_valid, tx_to_string,
};

#[test]
fn confirmations_window_and_unmined() {
    assert_eq!((MIN_RELAY_FEE_CONFIRMATIONS, FEE_CONFIRMATIONS), (1, 6));
    let fx = fixture();
    let collaterals = fx.json["collaterals"].as_object().expect("collaterals");
    let mut checked = 0;
    for (obj, _) in fx.objects.iter().filter(|(o, _)| object::object_type(o) == PROPOSAL) {
        let txid = object::txid_bytes(&obj.collateral_hash);
        let Some(c) = collaterals.values().find_map(|c| {
            let bytes = common::unhex(c["hex"].as_str()?);
            let tx = decode_tx(&mut bytes.as_slice()).ok()?.into_value();
            (object::txid_bytes(&tx.txid()) == txid).then_some(tx)
        }) else {
            continue;
        };
        let check = |mined, confirmations| {
            collateral_valid(
                obj,
                Some(FoundCollateral {
                    tx: &c,
                    mined,
                    confirmations,
                }),
            )
        };
        let unmined = check(false, 0);
        assert_eq!(
            (unmined.valid, unmined.missing_confirmations),
            (false, false),
            "{}",
            unmined.error
        );
        assert_eq!(unmined.error, format!("Collateral tx {} is not mined yet", tx_to_string(&c)));
        let short = |n: i32, verdict: &str| {
            format!(
                "Collateral requires at least 6 confirmations to be relayed throughout the network (it has only {n}), {verdict}"
            )
        };
        for (n, missing) in [(0, false), (1, true), (5, true)] {
            let v = check(true, n);
            assert!(!v.valid, "{n}");
            assert_eq!(v.missing_confirmations, missing, "{n}");
            let verdict = if missing {
                "pre-accepted -- waiting for required confirmations"
            } else {
                "rejected -- try again later"
            };
            assert_eq!(v.error, short(n, verdict));
        }
        let six = check(true, 6);
        assert!(six.valid && !six.missing_confirmations, "{}", six.error);
        checked += 1;
    }
    assert!(checked > 0, "a proposal with its collateral");
}
