//! Quirk 2 (R4 §1.15): `CheckTransaction`'s allowances key on `nType` alone. Types 6 and 7 may
//! have no inputs and no outputs, type 9 no inputs, and a type-5 coinbase may have a one-byte
//! scriptSig, whatever the version: also at versions below 3, which carry no payload and are not
//! special transactions.
//!
//! The verdicts are Core's (`data/checks_recorded.json`), and the node reproduces each.

#[path = "support/checks.rs"]
mod checks;
#[path = "support/checks_recorded.rs"]
mod checks_recorded;
#[path = "support/checks_replay.rs"]
mod checks_replay;
#[path = "support/checks_rows.rs"]
mod checks_rows;

use checks_replay::{consensus, core_verdict};

#[test]
fn quirk_02_check_transaction_type_keys() {
    for version in [3, 1] {
        assert_eq!(
            core_verdict(&format!("quirk 2: v{version} type 6 without inputs or outputs")),
            Ok(())
        );
        assert_eq!(core_verdict(&format!("quirk 2: v{version} type 9 without inputs")), Ok(()));
        assert_eq!(
            core_verdict(&format!("quirk 2: v{version} type 5 coinbase, 1-byte scriptSig")),
            Ok(())
        );
    }
    for version in [3, 2] {
        assert_eq!(
            core_verdict(&format!("quirk 2: v{version} type 7 without inputs or outputs")),
            Ok(())
        );
    }
    // The allowances are per type: type 9 still needs outputs, type 8 inputs, and other
    // coinbases two bytes of scriptSig.
    assert_eq!(core_verdict("type 9 without outputs"), consensus("bad-txns-vout-empty", ""));
    assert_eq!(core_verdict("type 8 without inputs"), consensus("bad-txns-vin-empty", ""));
    assert_eq!(
        core_verdict("quirk 2: v3 type 0 coinbase, 1-byte scriptSig"),
        consensus("bad-cb-length", "")
    );
}
