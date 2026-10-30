//! Scaffolds for the register quirks `dash-consensus` will implement and does not yet
//! (`docs/node-program/quirks.toml`; PLAN task O5). Each test is ignored with its owner task. The
//! owner replaces the body with assertions on Core-produced vectors, moves the test next to its
//! other tests if that reads better, removes the `#[ignore]` and sets the register's status to
//! "tested" in the same commit; `contrib/node/check-quirks.py` checks the two agree.
//!
//! Core locations are at the v24.0 pin (`v24.0.0-rc.3`).

/// Fails an ignored scaffold that is run anyway (`--ignored`): the quirk is not implemented yet.
fn scaffold(id: u32, owner: &str) -> ! {
    panic!("quirk {id} is a scaffold: {owner} replaces this test with Core-produced vectors")
}

/// Quirk 6: `SIGHASH_SINGLE` out of range hashes ONE; `FindAndDelete` on BASE scripts, empty
/// signatures included; the special-tx payload is in the sighash
/// (`src/script/interpreter.cpp:224,328,1354-1410,1512-1530`).
#[test]
#[ignore = "quirk 6: owner CS1"]
fn quirk_06_sighash_single_find_and_delete_payload() {
    scaffold(6, "CS1")
}

/// Quirk 33: `hash_serialized_2`'s VARINT precedence; unspendable outputs never enter the UTXO
/// set; coinstatsindex `unclaimed_rewards` can go negative (`src/kernel/coinstats.cpp:77`,
/// `src/index/coinstatsindex.cpp:205-210`).
#[test]
#[ignore = "quirk 33: owner CS5b"]
fn quirk_33_hash_serialized_2_varint() {
    scaffold(33, "CS5b")
}

/// Quirk 39: `SerializeScriptCode` writes a length counting the whole script but stops writing at
/// a truncated trailing push (`src/script/interpreter.cpp:1366-1395`). The reference
/// serializer's edge cases are in `test-vectors/src/script.rs`; the port's sighash comes with CS1
/// phase B.
#[test]
#[ignore = "quirk 39: owner CS1"]
fn quirk_39_script_code_truncated_push() {
    scaffold(39, "CS1")
}
