# V24A independent review, round 2 (focused)

Reviewed `node/v24a-dashcore-wire` at **`ea2dbf49364774884df1bfee64bb6825f40a41e1`**. Compared the original ten commits through `backup/v24a-r0` (`a22c7138`) with their rebased equivalents (`b17fef52^..be446b32`) using `git range-diff`, then reviewed the **23 repair commits** in `be446b32..HEAD`. The range-diff shows the original changes retained; the changed original patches accommodate changes already on integration, principally the hex API and surrounding tests.

Read `REVIEW-COMMON.md`, `REVIEW-V24A.md`, `PROCESS.md`, and both round-1 reports. Applied the `code-review-validator` review passes. References were Dash Core tags **v24.0.0-rc.3** and **v23.1.8**. No source changes, commits, pushes, or GitHub comments were made. Scratch evidence is under `/work/scratch/v24a-r2-gpt/`. The reviewed worktree is clean at the same HEAD.

**Result:** all round-1 major findings are closed. No new blocker or major was found. Not every minor/nit is implemented: the disposition table distinguishes fixes, documentation mitigations, and remaining follow-ups. Those remaining items were already nonblocking in round 1.

## Core parity and legacy regression evidence

### NetInfo extraction and canonical signing

Checked the implementation against `src/evo/netinfo.{h,cpp}`, `src/netaddress.{h,cpp}`, `src/serialize.h`, `src/streams.h`, and the provider serializers. Core sorts the purpose map and retains the first duplicate; unknown entry types consume one byte; service failures are caught at their actual stream position; unknown networks consume their address and port; failed all-or-nothing reads consume nothing; invalid services/domains become invalid entries; domain read failures propagate. Rust now preserves consumed wire bytes and uses the canonical object for signing. Service port zero is correctly left to subsequent NetInfo validation; it is not incorrectly added to `CService::IsValid`.

Built an independent C++ probe using Core's `GetTxPayload<CProRegTx/CProUpServTx>`, `SER_GETHASH` serialization and `SerializeHash`, and a Rust probe using the reviewed crate. The Core build's eight relevant source files listed above, including `providertx.h` and `bls.h`, were byte-compared with the rc.3 tag before using its libraries.

**2,356 differential cases: 2,118 typed, 238 raw, zero mismatches.** These include the committed provider vectors, all 12 crafted NetInfo vectors, 2,000 bounded randomized purpose maps/address/domain combinations, 288 legacy address/port combinations, and 24 existing-signature cases. Compared extraction success, complete canonical unsigned serialization, signing digest, wire round-trip and transaction hash. Randomized cases include duplicate/unknown purposes, unknown types/networks, wrong/noncanonical address lengths, invalid addresses, port boundaries, invalid/non-UTF-8/oversized domains, and NetInfo version short-circuits. This is a bounded differential check, not exhaustive fuzzing or contextual consensus validation.

The committed NetInfo suite additionally checks **five mined**, four rejected and three unparsable cases. Accepted ProRegTx signatures recover to the collateral keys, their sign strings match Core, and under `--all-features` accepted ProUpServTx BLS signatures verify over the canonical digest. Thus the tests exercise accepted noncanonical wire encodings, rather than merely asserting Rust canonicalization against itself.

Evidence: `differential.py`, `differential-inputs.json`, `differential-errors.json`, `differential.log`, `core_probe.cpp`, and `probe/src/main.rs` in the scratch directory.

### Legacy v1/v2 addresses: txids and existing signatures

The new `canonical_legacy_service` matches Core's `SetLegacyIPv6` → `CService::IsValid` → `NetInfoEntry` → `MnNetInfo::Serialize` path. v23.1.8 has the same relevant legacy normalization behavior. Invalid addresses serialize again as an entirely zero **18-byte** service, including the port. The check correctly treats IPv4-mapped addresses as IPv4 and clears unspecified/broadcast IPv4, unspecified/documentation IPv6, Tor v2 and internal addresses. It preserves valid addresses, including local/private addresses and valid addresses with port zero, which are separate validation questions.

The 288 combinations cover both provider types, payload versions 1 and 2, 18 address forms and ports 0/1/9999/65535. **Canonical signing bytes and hashes match Core in every case; full transaction bytes round-trip unchanged and txids equal an independent double-SHA256 of those bytes.** Valid original legacy fixture signing preimages remain byte-identical to the unsigned wire payload. Existing legacy external-collateral signatures pass `core_v24_payload_signatures`.

For the Core-produced no-service Evo registration `rpc_netinfo@203`, changed its wire address to six forms Core clears, at all four ports, retaining the existing collateral signature. All **24** variants have the original canonical signing preimage and collateral sign string. Their txids follow their distinct wire bytes. The change therefore corrects signing normalization without normalizing transaction serialization. A signature formerly produced over an invalid address's unnormalized bytes was already incompatible with Core; preserving that incorrect digest is not a compatibility requirement.

SML entry hashes now also use the canonical service, matching `CSimplifiedMNListEntry::CalcHash`; ordinary wire serialization remains separate. Existing Core ground-truth SML roots pass.

### qrinfo and fixture provenance

Recaptured the four committed qrinfo responses with the rc.3 release binary, from a private copy of `feature_llmq_rotation/node0`, using RPC/P2P ports **32950/32951**. **4/4 P2P messages and associated Core JSON are identical**, including both extra-share settings, one/two base blocks and the earlier-cycle response. The private dashd was stopped. No `/work/chains` or `/work/bench` instance was touched.

Evidence: `recapture_qrinfo.py`, `recapture-qrinfo.log`, and `qrinfo-recapture.json`. The tests check snapshots, embedded diffs, commitments, transaction preservation and SML roots. The mnlistdiff coverage now honestly requires IPv4/IPv6; broader ProTx address coverage is enforced separately and includes CJDNS.

## Every round-1 finding: disposition

Identifiers below retain each reviewer's numbering.

| Round-1 finding | Round-2 disposition and evidence |
|---|---|
| GPT **M1**, Opus **M1**: NetInfo map order/duplicates, tolerated entries and signing hash | **Closed.** Canonical sorted/first-wins map and signing serialization, consumed-byte preservation, exact stream-stop handling; independent differential probe and mined/signature-checked vectors above. |
| GPT **M2**: address normalization/domain bound | **Closed.** Core-equivalent address/domain checks and 253-byte cap; invalid entries preserve wire bytes and canonicalize to `Invalid`. |
| GPT **M3**, Opus **M2**: FFI tag/null-pointer contract | **Closed.** Raw and pre-special-version representations use `FFI_SPECIAL_PAYLOAD_TYPE_RAW = 0xffff`; provider tags 1–4 retain their non-null matching pointer. Regression through `FFITransactionRecord` verifies tag, pointers and preserved `tx_data`; FFI suite passes. `repr(C)` field layouts remain unchanged. |
| GPT **m1**: witness size/weight | **Closed.** Size/weight uses the same type-zero witness eligibility as encoding; special-transaction witness regression passes. |
| GPT **m2**: layout/address coverage and qrinfo | **Closed for the requested repair.** Explicit regular/Evo/shared/external-collateral layouts and all address kinds; crafted invalid boundaries; four independently recaptured qrinfo responses with both extra-share settings. The broader residual coverage gaps are listed under Opus m10. |
| GPT **m3**: failed regeneration children/provenance label | **Closed.** Saved child PIDs aggregate failure; runner checks the binary version against the selected tag; extraction/capture records actual `dashd -version` instead of a hardcoded release label. A release-version label is not a cryptographic attestation of an arbitrary caller-supplied binary. |
| Opus **m1**: eager BLS parse canonicity | **Documentation mitigation accepted in round 1, now present.** `TransactionType::consensus_decode` and CHANGELOG explicitly state this extraction divergence. It remains a permissive SPV parser, not Core's eager BLS decoder or a consensus-validation interface. |
| Opus **m2**: 253-byte domain bound | **Closed**, together with GPT M2. |
| Opus **m3**: CbTx compact-size MAX_SIZE | **Closed.** `0x02000000` accepted and `0x02000001` rejected; matches Core's range-checked `COMPACTSIZE`. |
| Opus **m4**: BIP152 short IDs | **Closed.** Version-1 compact blocks use `instance_hash()`, matching Core `blockencodings.cpp:33`; regression distinguishes the stable txid from the instance hash. |
| Opus **m5**, both reviewers' Platform rollout note | **Migration note closed; downstream migration still required before v2 emission.** CHANGELOG names the exact unsigned-withdrawal vote-extension call site, signature failure and persisted-key impact. Core signs full serialization with the 96-byte quorum signature zeroed. No Platform code was changed or tested in this review. |
| Opus **m6**: incomplete v3 FFI views | **Documentation mitigation closed.** Each affected FFI view explains missing v3 fields and where full transaction bytes are available. The fields are still not exposed through those views. |
| Opus **m7**: constructed states silently drop payload/panic | **Partially mitigated, not fully fixed.** Lower-version payload omission and `txid`/`instance_hash` panics are documented. Infallible payload-hash methods can still panic and have no corresponding panic contract; retained nit R3 below. |
| Opus **m8**: share-owner/operator/voting wallet matching | **Still open**, retained minor R1 below. Payout matching is correct but does not implement key-role tracking. |
| Opus **m9**: builder fee sizing | **Closed.** Uses actual versioned payload size plus conservative ECDSA and missing-join-signature allowances; the finalizer guard uses the same estimate. Eight-share and share-update regression tests pass. |
| Opus **m10**: vector gaps | **Partially closed.** qrinfo, CJDNS, adversarial Core verdicts and explicit layout/address guards are added. Multi-unlock/mixed-version roots, captured inventory 34 and real CbTx v1/ProUpRevTx v1 bytes remain absent; retained minor R2 below. |
| Opus **n1**: constructor CURRENT_VERSION constants | **Documentation mitigation closed.** Constants now describe the backward-compatible versions their constructors build, with Core's activated newer versions stated; constructors are not claimed to build activated v24 transactions. |
| Opus **n2**: inaccurate version/docs/test | **Closed.** Extraction versus validation distinguished, registration version comment corrected, size test uses v1. |
| Opus **n3**: unsupported constructed ProUp* versions | **Still open**, retained nit R4 below. Network decoding keeps these raw, so this does not change decoded transaction txids. |
| Opus **n4**: repeat serialization/allocation | **Unchanged optional optimization.** No correctness failure; no evidence warrants changing the raw-preservation check in this repair. |
| Opus **n5**: input-less type-zero BIP144 path | **Unchanged pre-existing decode divergence**, `transaction/mod.rs:693`. A zero-input classical transaction can be misinterpreted as witness framing; future node decoding must use its Core-compatible base-sdk path. This is outside the focused NetInfo repair. |
| Opus **n6**: remaining legacy bincode tests | **Closed.** CbTx v3 and ProUpServTx v2 legacy-layout and full-consumption checks pass with all features, alongside the earlier provider tests. |
| Both reviewers: timing-test recommendation | **Closed by integration's separate performance-test fix.** Wall-clock thresholds are opt-in through `RUN_PERF_ASSERTIONS`; workload/correctness tests remain. All 676 key-wallet unit tests pass here with the default setting. |

## Retained nonblocking findings

These are unfinished round-1 items, not new major regressions.

### R1 — Minor: shared masternode key-role matching remains incomplete

**Locations:** `key-wallet/src/transaction_checking/account_checker.rs:999`, `:1458`; `key-wallet/src/transaction_checking/transaction_router/mod.rs:63`.

A wallet holding a share's owner key, or an operator/voting key changed by ProUpSharedRegTx, still does not record the transaction in the matching provider-key account unless another matching path finds it. Only the top-level registration owner is consulted; shared update types route as fund-bearing transactions. **Fix:** track a wallet follow-up covering share-owner hashes and the type-12 operator/voting roles, with account-level routing tests. This was explicitly allowed as a follow-up in round 1 and does not block the wire-format repair.

### R2 — Minor: several Core differential coverage gaps remain

**Location:** `dash/tests/core_v24_vectors.rs:397` and the committed `core_v24_*.json` fixtures.

The asset-unlock commitment test still lacks a Core block containing multiple stable unlocks or a v1/v2 mixture; it cannot catch a regression in leaf ordering, reduction or v1 exclusion from that fixture alone. Inventory 34 lacks a Core P2P capture, and CbTx v1/ProUpRevTx v1 still lack release-generated positive wire fixtures in this suite. **Fix:** add these captures and coverage guards in a vector follow-up. The current root implementation agrees with Core's `cbtx.cpp:157–171` (block-order stable-unlock instance hashes); no implementation mismatch was found.

### R3 — Nit: payload-hash panic contracts remain incomplete

**Locations:** `provider_registration.rs:511`, `provider_update_service.rs:219`, `provider_update_registrar.rs:139`, all under `dash/src/blockdata/transaction/special_transaction/`.

A caller constructing a v3 provider payload without required fields can still panic when requesting the infallible payload hash or collateral sign string. Transaction hash methods now document that limitation, but payload hash methods do not. **Fix:** add the same panic contract to the payload-hash API or introduce a fallible validated construction/hash path. Decoded values did not hit this case in the differential tests.

### R4 — Nit: unsupported constructed provider versions still serialize differently from Core

**Locations:** `dash/src/blockdata/transaction/special_transaction/provider_update_service.rs:166`, `provider_update_registrar.rs:120` and the corresponding revocation encoder.

An explicitly constructed version-0 ProUp* object emits fields after the version, whereas Core's serializer stops after the two-byte version. The transaction decoder correctly preserves such unsupported payloads as raw, so existing decoded signatures/txids are unaffected. **Fix:** either reject unsupported versions on encoding or reproduce Core's early return and document the constructed-object contract.

## Validation

- `cargo test -p dashcore`: passed; 550 unit tests, all integration suites including the eight v24 vector tests, and doc tests.
- `cargo test -p dashcore --all-features`: passed; 597 unit tests, all integration suites and doc tests.
- `cargo test -p dashcore --all-features --test core_v24_vectors -- --nocapture`: **8/8 passed**, including BLS verification in the crafted NetInfo cases.
- `cargo test -p key-wallet-ffi --all-features`: passed; 247 unit tests and all integration/doc suites.
- `cargo test -p key-wallet --all-features`: passed; 676 unit tests and all integration/doc suites, including payload fee-sizing/finalizer tests.
- Independent Core/Rust probe: **2,356 cases, zero mismatches**.
- Independent qrinfo recapture: **4/4 byte-identical messages and JSON**; private node stopped.
- `cargo fmt --all -- --check` and `git diff --check be446b32..HEAD`: passed.

Logs are in the scratch directory. This focused round did not repeat round 1's historical-chain replay or run the whole workspace suite. It does not claim consensus validation parity for permissive BLS fields, unsupported constructed states, or the pre-existing classical witness compatibility path.

## Verdict

**APPROVE-WITH-NITS**

The major repair requirements are satisfied, including Core-equivalent canonical signing and preserved legacy txids/signatures. Keep the retained nonblocking items and the conditional Platform migration visible when merging; do not label every round-1 finding as fully implemented.
