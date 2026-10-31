# MG-04b-1b review (round 1, Sol)

## Findings

### M-1 — capability gate enables user sending before the ruled follow-up
- **Location:** `rust/crates/dw-engine/src/masternodes/shared.rs:144-146` (also asserted by `rust/bin/dwcli/tests/mn_shared_cli.rs:170-173`).
- **Scenario:** On the 1b head, a normal release build reports `shared_capability.can_send = true`, so dwcli/UI can expose `sign`, `send`, `abandon`, and `update-share`. DEC-678 explicitly rules that 1b must merge with `can_send` false behind one switch because D39 (duplicate net-info coverage) and M9 (the rc.3 envelope/check_registration oracle) remain open; those are closed only by MG-04b 1c, which then flips the switch. A user can therefore send a registration whose second/platform net-info collision is unchecked and without the required oracle coverage.
- **Suggested fix:** Keep the single production capability switch false on the 1b head (retain test-only override if needed), and add/retain the release test asserting false. Flip it to true only in 1c after D39 and M9 land.
- **Core reference:** rc.3 `src/evo/specialtxman.cpp:1231-1245` rejects duplicate service entries against every listed masternode net-info entry; the pinned summary currently exposes only the primary endpoint (design D39/D75).

## Verdict
CHANGES-REQUIRED (1 major, 0 minor, 0 nits)

## Checked and ran
- Read `docs/design/MG-04b-shared-masternodes.md` sections 5 and 12, D39 and D63–D75, and DEC-648/DEC-678.
- Compared shared ProRegTx v3, ProUpShareTx, ProUpSharedRegTx, and ProDisTx codecs, consent/signature hashes, inputs hashes, fee/change construction, and rule ordering/reasons against Dash Core `v24.0.0-rc.3` (`da567d7d`, including `src/evo/providertx.cpp`, `src/evo/specialtxman.cpp`, `src/evo/netinfo.cpp`, `src/evo/providertx.h`, and `src/qt/mnsharesession.cpp` / `sharedmndialogs.cpp`).
- Reviewed session and sigs JSON field order/types/refusals and the funding/payload envelope checks. D39's primary-endpoint-only gap was treated as ruled and not counted as an independent finding.
- Created the required reflink review copy at `/work/scratch/mg04b-1b-rv-sol`, using its own `target/` and `tmp/`.
- `cargo test -p dw-protx --all-features` — passed (96 unit tests plus all integration/oracle/no-operator-signing suites).
- `cargo test -p dw-engine --all-features masternodes::shared::tests` — started in the review copy; compilation was still running when the review was finalized.
