# MG-04b slice 1c review (r1)

Branch `dw/mg-04b-1c`, head `3224891e`, compared with `67da2b55`.

## Verdict

**CHANGES-REQUIRED**

Counts: **0 M, 1 m, 0 n**.

## Findings

### m-1 — D78 is not fail-closed on non-glibc builds

**Where:**

- `rust/crates/dw-protx/src/shared/netinfo.rs:87-116` (`parse_core_service`);
- `rust/crates/dw-engine/src/masternodes/shared/build.rs:75-108` (`participant_services_ok` and `net_info_of`);
- `rust/crates/dw-protx/src/shared/session/funding.rs:188-209` (`net_info_matches`);
- `rust/crates/dw-engine/src/masternodes/shared/store.rs:534-543` (`service_read`);
- `rust/crates/dw-engine/src/masternodes/shared.rs:466-506` (`join_shared` accepts the service and reserves the share).

The implementation hard-codes glibc's legacy `inet_aton_exact` behavior, but it has no target/libc gate. The same parser is used on every platform and is treated as the participant's acceptance test, the frozen-payload comparison, and the UI's advertised Core reading. D78 explicitly says that the glibc reading must fail closed on other libcs.

Concrete sequence: build the desktop for a non-glibc libc (for example musl), import a dash-qt draft whose `coreP2PAddrs` is `127.1:19999`, and call `join_shared`. `participant_services_ok` accepts that text through the hand-written glibc parser as `127.0.0.1:19999`; `join_shared` then runs `check_placeholder` and reserves the participant's share before any frozen payload is available. On a libc whose `getaddrinfo(..., AI_NUMERICHOST)` does not read `127.1` the same way, Core's `ExtNetInfo::AddEntry` has different behavior (or rejects the entry; rc.3 `src/evo/netinfo.cpp:500-551`, `src/netbase.cpp:44-59`). The wallet has therefore accepted and locked funds for a service interpretation it cannot prove is Core's. When a coordinator's frozen payload exposes the different interpretation, `approve_shared`/`net_info_matches` refuses later, leaving the share reservation to be manually discarded. `service_read` also presents the fabricated glibc reading on that build.

Fix by making the Core-text parser unavailable on unsupported libc/targets (return `None`/`false` from the central parser and all callers, including `service_read`) and retaining the strict plain-IPv4 coordinator path. Add a non-glibc compile/test case proving an incoming Core-form service is refused before `join_shared` reserves coins.

## What I checked

- Traced the enabled creation paths: `start_shared`/`join_shared` reserve under the coin fence; `lock_shared_terms` and `approve_shared` revalidate the frozen registration and own side; `sign_shared_funding` rechecks reservation, Core rules, consent hash and signs only own inputs; `send_shared_registration` requires all foreign inputs signed, rechecks the approval digest, signs coordinator inputs last, persists `out_at` before handoff, and resends only the saved bytes; `abandon_shared_session` spends one still-unspent reserved coin and keeps locks until settlement; `update_share` plans and signs its exact reward payload under the update lock and operation-row fence; `resend_operation` gates `update_share` rows before lease/charge. No key-rotation, dissolve, or standby sending path is exposed in this slice; their imports remain `unsupported`.
- D39: `check_registration` calls `check_net_info_unique` over `p.net_info.all_entries()` and compares every listed entry. `listed_net_info` retains all service and domain entries from every purpose; `check_net_info_unique` checks every payload entry against every other-collateral list entry. This matches Core rc.3 `src/evo/specialtxman.cpp:1231-1248` and the list construction in `src/evo/deterministicmns.cpp:459-475`.
- M9: `testdata/oracle/dashqt/mnshare_oracle.cpp` links rc.3's unmodified `src/qt/mnsharesession.cpp`; `rust/crates/dw-protx/tests/oracle_envelopes.rs` compares both serialized steps and 220 independent `fromJson` verdicts/read-backs. This is a real cross-implementation oracle, not a Rust self-comparison.
- Envelope/payload checks cover shares, owner/refund/reward scripts, voting/operator/reward/exit terms, Core-read net info, funding inputs/outputs, collateral index, lock time, consent hash, join signatures and funding scriptSigs. `check_consent` additionally requires special transaction version 3, final sequences, unchanged own row/contribution, no foreign wallet input, and the recorded debit bound.
- Contract v13 migration: `ShareTermsView.service_read` is an `Option`; serde supplies `None` when an old `approved_terms` JSON lacks the new field, so old approvals deserialize without silent re-approval. Approval still gates sending by consent hash.
- 1b r2 nits n-1..n-3 are closed in the contract/docs/tests: `resend_operation` refusal ordering is documented, the dwcli update-share assertion no longer pretends to test the switch, and the buried/conflict behavior is documented and tested.
- `git diff --check 67da2b55 3224891e` passed.

## Tests/build

I did not build or run cargo tests. Before starting a permitted build, `df -BG /work` reported only **223G free**, below the required 230G threshold, so I stopped at source review and left the reflink review copy at `/work/scratch/mg04b-1c-r1-sol`.
