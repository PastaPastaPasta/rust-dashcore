//! BL-61 (O2c Sol r1 F1): an allocation failure in Core is an oracle error in the overlay
//! comparison, never an agreement. A binary of its own, since the fault injection replaces the
//! binary's `operator new` (dash-oracle-sys `tests/support/alloc_fault.rs`).

use dash_oracle_sys::Pin;
use node_l1::overlay::{Case, MIN_VERSION, VERSIONS, compare_case};
use node_l1::{Divergence, Outcome};

#[path = "../../dash-oracle-sys/tests/support/alloc_fault.rs"]
mod alloc_fault;

/// Sol r1's getqrinfo (no base hashes, a zero request hash, `extraShare` 2: a strict flag) with
/// each of Core's allocations failing in turn. It was `BothReject` when the write-back's failed.
#[test]
fn allocation_failure_is_an_oracle_error() {
    let mut raw = vec![0; 34];
    raw[33] = 2;
    let case = Case {
        cmd: "getqrinfo",
        version: MIN_VERSION + VERSIONS - 1,
        legacy: false,
        max: [0, 0],
        raw,
    };
    assert_eq!(compare_case(&case, Pin::V24_0), Ok(Outcome::StrictFlag));
    let oracle_error = Err(Divergence {
        field: "oracle error",
    });
    let mut nth = 1;
    while let Some(outcome) = alloc_fault::failing(nth, || compare_case(&case, Pin::V24_0)) {
        assert_eq!(outcome, oracle_error, "Core's allocation {nth} failing");
        nth += 1;
    }
    // The input's copy, then the write-back's (the read of zero hashes allocates nothing).
    assert!(nth > 2, "{} allocations", nth - 1);
}
