//! Quirk 3 (R4 §1.15): from DIP0001, `ContextualCheckTransaction` rejects a transaction above
//! `MAX_STANDARD_TX_SIZE` (100,000 bytes) with `>`, where the mempool's policy check uses `>=`:
//! a block may hold a 100,000-byte transaction that relay refuses. Before DIP0001 only
//! `CheckTransaction`'s 1,000,000-byte limit applies.
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
use dash_consensus::tx::MAX_STANDARD_TX_SIZE;

#[test]
fn quirk_03_tx_size_consensus_limit() {
    assert_eq!(MAX_STANDARD_TX_SIZE, 100_000);
    assert_eq!(core_verdict("quirk 3: 100000 bytes at DIP0001"), Ok(()));
    assert_eq!(
        core_verdict("quirk 3: 100001 bytes at DIP0001"),
        consensus("bad-txns-oversize", "")
    );
    assert_eq!(core_verdict("quirk 3: 100000 bytes before DIP0001"), Ok(()));
    assert_eq!(core_verdict("quirk 3: 100001 bytes before DIP0001"), Ok(()));
    assert_eq!(core_verdict("1000000 bytes"), Ok(()));
    assert_eq!(core_verdict("1000001 bytes"), consensus("bad-txns-oversize", ""));
}
