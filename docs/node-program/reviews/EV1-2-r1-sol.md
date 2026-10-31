# EV1-2 review, round 1 (Sol)

Verdict: **ACCEPT**

Reviewed `node/ev1-2` at `0e8f2f3e9` against integration `fb76254e0`, focusing on production code in `dash-dml/src/specialtx/` and the D29 transaction-verdict leg.

## Findings

None (0 M, 0 m, 0 n).

The version maximum matches Core for all deployment combinations: 1 before V19, 2 with V19 only, and 3 with V19 plus V24; V24 alone does not raise it. Types 10–12 are admitted through the same deployment maximum and then apply their own current-version check, matching Core when V24 is inactive or active. `PayloadSlot`/`BlockPayloads` preserve the decode-complete distinction and transaction-position indexing.

The payout and share validators match Core's check order, result classes, boundaries, duplicate checks, script forms, destination-reuse checks, reward sums, amount sums, and early-penalty rules. The provider trivial validators match the rc.4 order and reasons, including the rc.4 removal of the NetInfo entry loop. The shared payload checks use the same version and signature-count bounds.

The ECDSA helpers match Core's compact-signature behavior: unrestricted recovery for `VerifyHash`, canonical header/low-S checks for `VerifyHashCanonical`, message magic hashing, and the per-type payload preimages. The preimage builders re-encode decoded payloads as Core's `SerializeHash` paths do. The `SigVerifier` interface leaves pairing evaluation injectable while retaining the scheme argument and decode-bearing signature/key values. Operator-key decoding is selected by payload version in the underlying payload decoder, including legacy ProReg/ProUpReg and basic ProUpSharedReg forms.

The D29 leg constructs the same ProReg/ProUpServ payload fields, invokes Core and the Rust checks on the same serialized transaction, and compares the rejection class/reason or the documented next Core step.

## Validation run

- `CARGO_TARGET_DIR=/work/scratch/ev1-2-rv-sol1-target CARGO_BUILD_JOBS=12 cargo test -p dash-dml --all-features` (204 unit tests passed, 2 ignored; integration tests and doctests passed).
- `git -C /work/scratch/ev1-2-frozen diff --check fb76254e0 0e8f2f3e9 -- dash-dml/src/specialtx dash-dml/src/specialtx/network dash-dml/src/specialtx/reasons node-fuzz/src/models/netinfo.rs node-l1/tests/l1_specialtx_sig.rs` (clean).
- Manual source comparison against Dash Core v24.0.0-rc.4 (`49caf372bb`) in `src/evo/providertx.{h,cpp}`, `src/evo/specialtxman.cpp`, and `src/evo/netinfo.cpp`.
