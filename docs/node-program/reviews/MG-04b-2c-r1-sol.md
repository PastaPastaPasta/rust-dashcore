# MG-04b-2c r1 — Sol review

**Verdict: ACCEPT**

No correctness findings (0 major, 0 minor, 0 nit).

## What I checked

- Read the complete `dw/mg-04b-2c` diff from `e521c935` to `a126d989`, including the new engine session, `dw-protx` builder/check/combine code, CLI/facade wiring, persistence, tests, contract/design rows, oracle vectors, and regtest additions.
- Traced the funds-moving path in [`rust/crates/dw-engine/src/masternodes/shared/together.rs`](/work/home/agent/workspace/dw-wt/mg-04b-2/rust/crates/dw-engine/src/masternodes/shared/together.rs): `start_together` (572–637) → import (639–742) → approve (744–806) → combine/send (808–896). The request is re-derived byte-for-byte from the registration and fee, the sign hash commits to the share count, each accepted signature is checked against its share owner, and send runs the complete Core-shaped dissolution checks again before the durable out marker and hand-off.
- Compared [`rust/crates/dw-protx/src/shared/together.rs`](/work/home/agent/workspace/dw-wt/mg-04b-2/rust/crates/dw-protx/src/shared/together.rs) with Dash Core rc.3 `PrepareSharedDissolution` and `InsertUnanimousSignatures`/`CombineShared`: output order, actor fee, fee ceiling, collateral/input template, lock/sequence, empty request signatures, share-order insertion, and missing/duplicate handling agree. The `shared_together.json` oracle and `oracle_shared.rs` cover the prepared/sign hash/combined bytes and Core refusal mutations.
- Checked D23 in both directions through `check_no_pending_maintenance` and the together start/approve/send gates, the relay-floor checks for the exact combined size, actor-fee charging/acceptance, dust-output refusal, and the `foreign`/`together` operation debit behavior on send and resend.
- Checked session derivation and restart persistence in `derive_stage` (257–303), including sent/confirmed/conflicted, collateral-spend detection, delisting, and the documented D94/D95 residual behavior.
- Compared import behavior with dash-qt `addSignatures`: invalid entries are skipped with a warning, byte-identical repeats are ignored, conflicting duplicates are skipped, and non-array `signatures` yields `NoApprovals` while retaining the request.
- Manager rulings: the serde JSON stage round-trip is an acceptable substitute because this slice adds no Swift `SharedStage` decoder or enum variant; the test still pins the wire encoding. Skipping malformed/bad entries is correct and required for dash-qt interoperability; the valid entries remain usable and final send re-verifies every signature.

## Validation

- `git diff --check e521c935 a126d989` passed.
- Started the focused command, after checking the required host conditions (`df -BG /work`: 280G free; 5-minute load: 11.49):
  `CARGO_BUILD_JOBS=8 cargo test -p dw-engine test_QT_127_dissolve_together --all-features`
  The command completed successfully (exit 0).
