//! Scaffolds for the register quirks whose owning crate has no home for them in this workspace
//! (`docs/node-program/quirks.toml`; PLAN task O5):
//! - `dash-evo` (EV*, LQ*) and `dashd-chainstate` (CH7a) do not exist yet;
//! - `dash-params` (BS5, quirk 34) exists in the base SDK, but the fork stays upstream-shaped
//!   (DEC-86), so its test tree carries no node scaffolds. A base SDK test that covers a quirk is
//!   registered as `base-sdk:<path>::<fn>`, as quirks 22, 35 and 37 are.
//!
//! Each test is ignored with its owner task. The owner replaces it with assertions on
//! Core-produced vectors in the owning crate, deletes the scaffold here and sets the register's
//! status to "tested" in the same commit; `contrib/node/check-quirks.py` checks the two agree.
//!
//! Core locations are at the v24.0 pin (`v24.0.0-rc.3`).

/// Fails an ignored scaffold that is run anyway (`--ignored`): the quirk is not implemented yet.
fn scaffold(id: u32, owner: &str) -> ! {
    panic!("quirk {id} is a scaffold: {owner} replaces this test with Core-produced vectors")
}

/// Quirk 28: Rotation: break on a missing snapshot, swallowed AddMN exceptions, the skip-list
/// first-index-0 encoding, skipRemovedMNs from V19 (or on testnet) (`src/llmq/utils.cpp:365-460`).
#[test]
#[ignore = "quirk 28: owner LQ1b"]
fn quirk_28_rotation() {
    scaffold(28, "LQ1b")
}

/// Quirk 29: V20 boundary: the first V20 cycle uses the base−8 list with the old modifier; the
/// modifier reads the work block's coinbase ChainLock from disk (`src/llmq/utils.cpp:87-115`).
#[test]
#[ignore = "quirk 29: owner LQ1a"]
fn quirk_29_v20_boundary_modifier() {
    scaffold(29, "LQ1a")
}

/// Quirk 30: The rotated signer index is (2^n − 1) & (GetUint64(3) >> (64 − n − 1)), n =
/// log2(active quorums): bits 58–62 for 32 quorums, not the top n bits
/// (`src/llmq/quorumsman.cpp:738-745,810-817`).
#[test]
#[ignore = "quirk 30: owner LQ1a"]
fn quirk_30_rotated_signer_index_bits() {
    scaffold(30, "LQ1a")
}

/// Quirk 31: MNHF: unknown bits are valid; any mined platform-type quorum may sign; the requestId
/// carries the bit as int64; historic bit 10/11/12 signals (`src/evo/mnhftx.cpp:32-35,95-155`).
#[test]
#[ignore = "quirk 31: owner EV3"]
fn quirk_31_mnhf_signals() {
    scaffold(31, "EV3")
}

/// Quirk 34: Hard-coded pins: bad-sb-start (614820), bad-dip3-enf-block (1047200), testnet
/// LLMQ_25_67 enabled from pindexPrev height 847000 (`src/chainparams.cpp:171-172,186-187`,
/// `src/validation.cpp:2621,6670`, `src/evo/deterministicmns.cpp:777`).
///
/// BS5's `hard_coded_pins` test (`pkgs/params/tests/builder.rs`) is on `node/base-sdk` but not at
/// the pinned revision; when the pin moves, register it as `base-sdk:<path>::quirk_34_<slug>` and
/// delete this scaffold.
#[test]
#[ignore = "quirk 34: owner BS5"]
fn quirk_34_hard_coded_pins() {
    scaffold(34, "BS5")
}
