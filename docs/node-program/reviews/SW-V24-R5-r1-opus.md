# SW-V24 R5 (EV-RC4) — review r1 (Opus)

| | |
|---|---|
| Target | rust-dashcore `node/ev-rc4`, range `88b37e6c3..15edfad82` (17 commits, incl. signed merge 141ee0528 of `node/swv24-pinmove` a6bf09a29) |
| Oracle | Core `375c64d2317` (`node/ev-rc4-oracle`) = `dd440ca847` + one commit in `src/test/oracle` |
| Spec | SW-V24-rc4-pin-delta.md rows R4/R5, §4.3, §5 items 1–8; `SW-V24-rc4-pin-delta/ce.md` CE4-01…05; D1–D6, D5b; DEC-461, DEC-468, DEC-472 |
| Core sources | rc.3 `da567d7db` (0364ed626e), rc.4 `0e7382df80` |
| Reviewer worktree | detached copy at 15edfad82 under /work/scratch/ev-rc4-opus-r1 (own target, CARGO_BUILD_JOBS=12); deleted after the review |
| Date | 2026-10-26 |

## Verdict

**ACCEPT with minor changes.** No Blocker or Major. Every CE4 row in R5's scope matches rc.4 when traced through Core's code: reject strings, reject order, the pre-V24/V24 split and the checked-arithmetic order. The oracle shim commit is sound, and pins.txt, the archives and the SB4 re-record agree. Of 17 hand mutants, the tests kill every reachable one. Four Minors are about test placement and how the interim workarounds are labelled; none of them changes a verdict.

## Findings

### Blocker
None.

### Major
None.

### Minor

**m1. The rc.4 purpose guard on the build/check path has no dash-dml test.** `dash-dml/src/build/mod.rs:235` (`payload`) and `:251` (`net_info_reads`).
Two mutants survive the whole dash-dml lib suite (all tests pass):
- mutant 4: `net_info_reads` always returns true;
- mutant 5: the ProRegTx offset is 40 instead of 42.

Only node-l1's oracle-backed `extended_net_info_matches_cores_verdict` kills them (`l1_dml_block.rs`, `oracle` feature). So a plain `cargo test -p dash-dml` cannot see CE4-01 on the build and check path, and the fixed offsets 42/36 rest on that one test.
**Fix:** add a dash-dml unit test that crafts a v3 ProRegTx and a v3 ProUpServTx with a repeated purpose, a descending pair and an unknown purpose. `payload(tx)` must return `None`, and `build_new_list_from_block` must give `bad-protx-payload`. Add an ascending control that reads. If base-sdk's decoder refuses the crafted bytes first, assert that, so the test turns into the R6 tripwire (see m2).

**m2. The interim workarounds are not all listed or marked, and one has no tripwire.** DEC-472 names three: conformance `BASE_SDK_RC3_PURPOSES`, mnlist `core_error`, and dash-dml `build::net_info_reads`.
- `node-fuzz/src/models/netinfo.rs:85` filters with `ext_purposes_in_order` before base-sdk's `NetInfoV2::decode`. That is the same workaround, but it is not on DEC-472's list and it is not marked interim. After R6 it is redundant.
- `net_info_reads` (`build/mod.rs:251`) and the public `ext_purposes_in_order` (`dash-dml/src/netinfo/mod.rs:172`) say why they exist, but not that R6 removes them. Nothing fails once base-sdk turns strict: the guard just goes dead. By contrast, the conformance list (`conformance/src/verbose.rs:1947`) asserts that base-sdk still reads the five cases, and so trips at R6.
- `core_error` (`dash-oracle-sys/tests/mnlist.rs:62`) is keyed to re-transcribing `transcribed/evo_snapshot_tests.json` at rc.4 ("records made at rc.4 carry it themselves"), not to base-sdk. R6's re-pin will not make it removable; a re-transcription will. Its tripwire asserts the rc.3 record text, which is right for that trigger.

**Fix:**
- Put an `// Interim until R6 (DEC-472): …` marker on `net_info_reads`, `ext_purposes_in_order` and the node-fuzz call.
- Add the node-fuzz site to R6's removal list.
- Give `net_info_reads` a tripwire (the m1 test asserting that base-sdk still decodes the repeated-purpose payload).
- Record that the `core_error` override goes with the rc.4 re-transcription of `evo_snapshot_tests.json` (R6 or a named row), and either do that now or name its owner.

**m3. The CE4-03 L0 two-fault vector is missing.** ce.md CE4-03 and §5 item 8 ask for a v3 ProRegTx with an unknown-type entry and `nOperatorReward` > 10000, which rc.4 rejects as `bad-protx-operator-reward` (rc.3: `bad-protx-netinfo-bad`).
The branch moves the single-fault "E checked" seeds from T11/T7 to C4/C2 (`node-l1/tests/l1_dml_block.rs:436-437`). Re-adding rc.3's entry loop (mutant 16) is killed by 12 node-l1 tests, but only through the step label each one asserts. No test pins the *reason* precedence, which is the observable change.
**Fix:** add the vector to `check_layer_mutant_inputs_compare_exactly`, or as its own row checked against the oracle: the E seed's ProRegTx with operator reward 10001. Expect `bad-protx-operator-reward` at T12 from both Core and the check layer.

**m4. `cargo test -p dash-conformance --features bsdk-judges` is red (pre-existing, not R5).** `conformance/tests/fixtures.rs:239` `unknown_special_type_is_unsupported_by_both_libraries` reports two unexpected `bsdk.payload` mismatches ("chain payload not accepted in full (type 10 basic/legacy)"). It fails the same way at base 88b37e6c3, and passes without `bsdk-judges`. It is not R5's regression, and it is recorded here because the brief asked for a conformance run.
**Fix:** the owner of BS4b's judges (`bsdk-judges`) adds `bsdk.payload` to the test's expected set, or the judge skips type 10. Out of R5's scope; backlog it.

### Nit

- **n1. Intermediate commits pair ABI 29 with an ABI 28 archive.** `dash-oracle-sys/src/ffi.rs:11` becomes 29 in bb4ef7eba. But pins.txt points at 77dbec8242 (ABI 28) through 6327dfb7c, and at dd440ca847 (ABI 28) in the merge 141ee0528. Only 15edfad82 pins 375c64d (ABI 29), so every oracle test is red on bb4ef7eba…141ee0528. This only matters for bisection. Fix if the train keeps per-commit history: move the ABI bump into 15edfad82, or note it in the train record.
- **n2. ce.md CE4-02 says "Regtest and devnet don't require routable addresses".** Devnet does: `chainparams.cpp` rc.4:687 sets `fRequireRoutableExternalIP = true`. The code and `rfc1918_is_never_routable` correctly expect `NotRoutable` on devnet. Fix the design text.
- **n3. The `dash-oracle-sys/README.md:37` heading says "Entry points (ABI 26)".** The ABI is 29. It was stale before this branch; update it with the table rows this branch already touched.
- **n4. Rows owned elsewhere.** CE4-18 is listed under §5 item 8 (EV2b's `build_new_list_from_block` exposing begin/apply/finish phases), and so is CE4-06's dash-mnlist "unconnected-block read never shadows a connect" unit test (R19 calls it "the dash-mnlist cache unit test (§5 item 8)"). Neither is in this branch, and neither is in R5's §6 row (CE4-01…05). Confirm that MN1 owns CE4-18 and ND1/R19 owns the CE4-06 test, so neither falls between rows.

## Checks with no finding

### 1. Parity, traced through rc.4's code

- **CE4-01: strict purposes** (`netinfo.h` rc.4:422-437).
  - The dash-dml record reader (`codec.rs` `Reader::net_info`) rejects an unknown or non-ascending purpose right after reading the purpose byte and before the group's CompactSize. That matches Core's order, so the `duplicate-group` budget row now fails before the second group is charged. Core's text is used: "ExtNetInfo: purpose unknown or out of order".
  - The `dash` crate's SML/payload `ExtNetInfo` decode does the same. `canonical()` no longer dedups, which is right because decoded values are now unique and ascending.
  - `IsValidPurpose` is {0, 1, 2}, matching `NIPurpose::Unknown`. A purpose equal to the last one is refused (`<=`). An empty group still reads, and `Validate` gives Malformed, as at rc.3.
  - Pre-V24: `GetValidatedPayload` (specialtxman.cpp rc.4:1158-1185) checks the payload (GVP 2) before the version (GVP 3). The node-l1 test asserts `bad-protx-payload` at GVP 2 for both V24 states, and the oracle agrees.
- **ProTx layout behind `net_info_reads`.** CProRegTx is nVersion u16, nType u16, nMode u16, then a 36-byte outpoint, so the offset is 42. CProUpServTx is nVersion, nType (version ≥ 2), then a 32-byte proTxHash, so the offset is 36 (`providertx.h` rc.4:162-183, 269-289). ExtNetInfo implies version 3, so nType is always present.
- **`Validate()` restructure** (netinfo.cpp rc.4:585-623). Dropping the unknown-purpose Malformed is right, since it is now unreachable. Dropping the per-entry `IsTriviallyValid` gives the same status for every decoded entry: Unserialize clears an invalid service or domain, so `GetAddrPort`/`GetDomainPort` return a value only for a valid one, and a cleared entry still gives Malformed. `ProcessCandidate`'s duplicate check over `GetEntries()` matches add.rs.
- **CE4-02: RFC1918** (netinfo.cpp rc.4:294, 456). `IsRFC1918() || !IsRoutable()` is equivalent to `!IsRoutable()` with `fAllowPrivateNet = false` (netaddress.cpp `IsRoutable`; the Rust `is_routable` is line for line the same). `is_routable(false)` is therefore exact for both validators and for AddEntry. Removing `NetInfoConfig::allow_private_net` makes the compiler catch every user. The ev2b consumer runs on regtest, so nothing changes there.
- **CE4-03: reject order.** The rc.4 `CProRegTx::IsTriviallyValid` and `CProUpServTx::IsTriviallyValid` match the node-l1 tables without T11/T7. `CheckProviderNetworkFields` → `IsNetInfoTriviallyValid` → `CheckNetInfo` (Malformed gives `bad-protx-netinfo-bad`, TX_BAD_SPECIAL) is C4 for ProRegTx and C2 for ProUpServTx. The multiset of reject strings in specialtxman.cpp is identical between rc.3 and rc.4.
- **CE4-04: diff mask** (dmnstate.h rc.4:259-281). The Rust rule is `GetHash() !=` (captured-scheme bytes) or `(a.nVersion == LegacyBLS) != (b.nVersion == LegacyBLS)`, via `scheme_for(v) = from_legacy(v == 1)`. Version 2→3 is correctly not a scheme change. Dropping the v23 point clause (`operator==`) is correct, because rc.4 has no such clause. nVersion is still forced when pubKeyOperator or netInfo is set.
- **CE4-05: checked sums** (creditpool.cpp rc.4:43-53, 297-350; specialtxman.cpp rc.4:1096-1127).
  - `GetDataFromUnlockTx`: the fee plus `CheckedAdd` of each output, which gives `failed-creditpool-unlock-overflow`.
  - `Lock`: `CheckedAdd`, which gives `failed-creditpool-lock-overflow`.
  - `Unlock`: `!total || *total > currentLimit` gives `failed-creditpool-unlock-too-much` before the duplicate-index check. The index is not taken, and `sessionUnlocked = *total`.
  - `GetTotalLocked`: `locked - sessionUnlocked` first, then `+ sessionLocked`, then `+ platformReward`. `checked_sub` equals `CheckedAdd(x, -u)` because u ≥ 0. A throw is caught as `failed-checkcreditpooldiff`, BLOCK_CONSENSUS, with no debug message; Rust matches.
  - `PoolError` Display still matches `tx_state.ToString()`. The new `UnlockDataReason::Overflow` has no persisted codec that could break (grep: no store codec for `PoolError` in this tree).
  - The unchecked sums left in `ConstructCreditPool` (`latelyUnlocked`, the block's unlocked sum) are documented as wrapping, and no checked chain reaches them.

### 2. Missed paths: `git diff da567d7db 0e7382df80 -- src/evo src/llmq src/netaddress.cpp src/netbase.cpp`

| File | Hunk | Row / disposition |
|---|---|---|
| evo/creditpool.cpp, .h | CheckedAdd ×3, `GetTotalLocked` out of line | CE4-05: done |
| evo/deterministicmns.cpp:729, 753 | `emplace` → `insert_or_assign` | CE4-06: not R5 (R19/ND1, see n4) |
| evo/deterministicmns.cpp:1232-1266 | repair errors | CE4-12: RPC, not R5 |
| evo/dmnstate.h:259-270 | scheme clause | CE4-04: done |
| evo/netinfo.cpp:294, 456 | RFC1918 | CE4-02: done |
| evo/netinfo.cpp:383-400, 442-447, 549-559 | `m_all_entries` removed | not delta (add.rs updated) |
| evo/netinfo.cpp:591-621 | `Validate` restructure | not delta for decoded values (checked above); unknown-purpose branch dropped in Rust |
| evo/netinfo.h:400-455 | strict reader, cache removal | CE4-01: done |
| evo/providertx.cpp:319-326, 425-437 | entry loop removed | CE4-03: done |
| evo/providertx_service.cpp | shared_combine | CE4-13: wallet RPC, not R5 |
| evo/specialtxman.cpp, .h | #7909 phase split, `CMNListBuilder` | not delta for connect (reject strings identical); CE4-18 phases: n4 |
| llmq/net_dkg.cpp:851-857 | `const auto` session (#7824) | not delta (mn-overlay §3) |
| llmq/net_signing.cpp:420 | `badSources` | MN4-10: SG1b, not R5 |
| netaddress.cpp, netbase.cpp | — | no change between the tags |

Other readers of ProTx payloads that bypass `payload()`:
- `dashd-p2p/src/processing/bloom.rs` and `dash-filters/src/elements.rs` read base-sdk's lenient decode directly. At rc.4 such a transaction is invalid everywhere: `bad-protx-payload` with V24 active, and version 3 is refused before V24. So it never reaches a valid block or the mempool. Not applicable now; R6's strict reader closes it.
- The SPV bridge decodes through `dashcore` first, which is now strict.

### 3. Tests

Hand mutants, env-gated in one build each:

| # | Mutant | Killed by |
|---|---|---|
| 1 | record reader skips the purpose rule | `records_read_purposes_strictly` |
| 2 | repeats allowed | codec + netinfo `purposes_read_strictly` |
| 3 | unknown purpose allowed | the same, + `validate_direct_statuses` |
| 4 | `net_info_reads` always true | **survives dash-dml**; killed by node-l1 `extended_net_info_matches_cores_verdict` (m1) |
| 5 | ProRegTx offset 40 | as 4 |
| 6 | `is_routable(true)` | 6 netinfo tests incl. `rfc1918_is_never_routable` |
| 7 | add before subtract | `the_total_is_checked`; L1 `checked_sums_agree`, `every_seed_agrees` |
| 8 | wrapping total | the same |
| 9 | lock wraps | `diff_sums_are_checked`; L1 both |
| 10 | unlock session sum wraps | the same |
| 11 | unlock amount wraps | `diff_sums_are_checked` (L1 survives: `CheckTransaction` keeps one transaction within MAX_MONEY, so the oracle cannot reach it; expected) |
| 12 | no scheme clause | `a_scheme_change_with_equal_bytes_is_recorded` |
| 13 | any version change records the key | the same (2→3 control) |
| 14 | overflowing total as a balance mismatch | `the_total_is_checked`; L1 both |
| 16 | rc.3 entry loop back | 12 node-l1 tests (step labels, see m3) |
| 17 | `dash` crate decode not strict | dash `purposes_read_strictly`, `v3_payload_hash_…`, `core_v24_netinfo`, `core_v24_netinfo_entries` |

- None of the new tests is tautological. The oracle tests (`netinfo_domain_entry.rs`, the `netinfo.rs` deser rows, `credit_pool_checked_sums`) assert Core's own verdicts, with expectations cited from Core's source. The Rust tests assert the same vectors on the Rust side.
- No `#[ignore]`, `should_panic` or skip gate was added in the range. The 36 ignored tests in the run are all pre-existing quirk-owner entries.
- D1–D6 and D5b are in the dash-dml codec tests, with the first entry's offset "byte 4" and the reader stopping inside the first entry. They are also in the oracle test `domain_first_entry_vectors`, and the L1 transaction-level test covers D5 and D5b. The CE4-01 vector pair (repeated and descending) is in all three. The CE4-02 RFC1918 table is in Rust (`rfc1918_is_never_routable`, four networks, range edges) and in the oracle (`addr_vals_main` under `-allowprivatenet`).

### 4. Oracle shim commit 375c64d2317 (`dd440ca847..375c64d2317`, `src/test/oracle` only, +45/−32)

- **ABI 29.** `CheckCreditAmounts` keeps its refusals only where rc.4 still sums unchecked:
  - the disk blocks (locks and unlocks ≤ MAX_MONEY, CbTx balance in money range);
  - on an accepted step at V20, the block's unlocks and CbTx balance (`check_locks=false`) and `prev.latelyUnlocked`.
- The money-range refusal of `prev.locked` and `prev.currentLimit` is lifted. I checked that against `ConstructCreditPool` (rc.4:173-247): `prev.locked` is only compared with 0, `prev.currentLimit` is not read, and the sums in `CCreditPoolDiff` are all checked. So the lift exposes no undefined behaviour.
- `expected` is absent when `GetTotalLocked` throws `overflow_error`, which matches the Rust `total_locked() -> Option`.
- The header text and the Rust doc comments (`payments.rs`, the README row) agree.
- pins.txt: `v24.0 375c64d2317a… node/ev-rc4-oracle 651d53cb…bf 668d297e…59`. The branch head is 375c64d. `sha256sum -c` passes for every file in `/work/cache/oracle/375c64d…/{plain,fuzz-nocmp}/SHA256SUMS`, and the archive hashes equal the pins.txt columns.
- SB4 re-record: all 54 `core-scenarios/*.json.gz` were decompressed and diffed with sorted keys. The only changes are `core_tag` (rc.3 → rc.4), `core_commit` (77dbec8242 → 375c64d231), `binary_sha256`, the input-archive hashes, and `instructions_u` in the two bench files (−0.09% / +0.09%). No vector content changed.
- Pin-move rider files:
  - `REF_COMMIT` = `v24.0.0-rc.4^{commit}` 0e7382df80d0….
  - `node/func-compat-v24.0-rc4` (f38372537f) is on rc.4.
  - The asserted-log list changes are line moves plus three new `p2p_instantsend.py` rows (MN4-09).
  - `setup-dashd.py` names 24.0.0-rc.4.
  - The `dash` core-vectors README records the rc.4 release-tarball sha256 and the regenerated variants; the `mined` rows are now Core's own payloads.

### 5. Interim workarounds

All three named in DEC-472 are small and local. The conformance one is self-removing, and `core_error` is pinned to the rc.3 record text. Labelling and the missing tripwire: m2.

### Test run

`cargo test --no-fail-fast -p dash-dml -p dash-evo -p dash-oracle-sys -p node-l1 -p dash-conformance -p dashcore --features dash-oracle-sys/oracle,node-l1/oracle,dash-conformance/bsdk-judges` at 15edfad82 (no fuzzers, gates or PF0):
- **1874 passed, 1 failed, 36 ignored** (pre-existing).
- The one failure is m4 (`bsdk-judges` only, also red at 88b37e6c3).
- The R5-specific tests all pass, including `l1_payload::diff_payload` in this run: `extended_net_info_matches_cores_verdict`, `checked_sums_agree`, `nested_budget_verdicts_match_the_oracle`, `fuzz_targets_agree_on_their_public_seeds`, `canonical_reads_match_core_tests`, `domain_first_entry_vectors`, `purposes_must_ascend`, `credit_pool_checked_sums`, `core_v24_netinfo` and `unreadable_payloads_flatten_like_core`.
