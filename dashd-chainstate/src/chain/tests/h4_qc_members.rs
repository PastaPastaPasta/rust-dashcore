//! CH7b-2's §7.2 rows (C11, D3 turned around): H4's commitment checks that read the quorum's
//! members, which G1q refused at H4's entry until CH7b-2, now run with them through the
//! attempt's binding. The world is mineable's: LLMQ_TEST's quorum based at 48, its commitment
//! mined at 58, over lists holding masternodes 1 to 9 (`llmq/commitment.cpp` v24.0.0-rc.4).

use dash_evo::llmq::commitment_version;

use super::mineable::{MINED, Pool};

/// One block per row, judged by H4 and rolled back:
/// - the commitment, every member valid and two signing, passes `Verify(checkSigs = false)` at
///   step 4 and step 6, so without signature checks the block connects;
/// - with the indexed version, which only a rotated type's commitment takes, and index 0, the
///   basic scheme's key still decodes and `Verify`'s version check fails (`:130-136`): step 4's
///   `bad-qc-invalid` (`:301-305`);
/// - with a payload height other than the block's, step 4's `bad-qc-height` (`:275-279`);
/// - with the signature checks, step 6 queues the members' aggregated signature over their
///   operator keys (`:81-88`), which the fixture's signatures fail: `bad-qc-invalid`
///   (`llmq/blockprocessor.cpp:267-283`).
#[test]
fn h4_judges_a_commitment_against_its_quorums_members() {
    let p = Pool::new();
    let good = p.qc(2);
    let mut indexed = good.clone();
    indexed.version = commitment_version(true, true);
    indexed.quorum_index = Some(0);
    let rows = [
        p.verdict(good.clone(), MINED, false),
        p.verdict(indexed, MINED, false),
        p.verdict(good.clone(), MINED - 1, false),
        p.verdict(good, MINED, true),
    ];
    assert_eq!(
        rows,
        ["connects", "step 4: bad-qc-invalid", "step 4: bad-qc-height", "bad-qc-invalid"]
    );
}
