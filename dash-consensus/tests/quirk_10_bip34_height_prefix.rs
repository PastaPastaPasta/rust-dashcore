//! Quirk 10 (R4 §1.15): from BIP34 until DIP0003, `ContextualCheckBlock` requires the coinbase
//! scriptSig to start with the bytes of `CScript() << height`, compared as a prefix. For heights
//! 1 to 16 that is the opcode `OP_1`..`OP_16`, so a minimal push of the number (`01 05`) is
//! rejected there; from 17 the two forms are the same bytes. From DIP0003 the height is in the
//! CbTx payload and the prefix is not checked.
//!
//! The verdicts are Core's (`data/checks_recorded.json`, regtest, where BIP34 is active from
//! height 1), and the node reproduces each.

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
fn quirk_10_bip34_height_prefix() {
    let mismatch = consensus("bad-cb-height", "block height mismatch in coinbase");
    for height in [1, 5, 16] {
        let row = |form| core_verdict(&format!("quirk 10: regtest height {height}, {form}"));
        assert_eq!(row("CScript() << height"), Ok(()), "{height}");
        assert_eq!(row("a one-byte push"), mismatch, "{height}");
    }
    let row = |form| core_verdict(&format!("quirk 10: regtest height 17, {form}"));
    assert_eq!(row("CScript() << height"), Ok(()));
    assert_eq!(row("a one-byte push"), Ok(()));
    // From DIP0003 only the CbTx type is checked.
    assert_eq!(core_verdict("regtest DIP0003 coinbase without a height, a CbTx"), Ok(()));
    assert_eq!(
        core_verdict("regtest DIP0003 coinbase without a height, not a CbTx"),
        consensus("bad-cb-type", "coinbase is not a CbTx")
    );
}
