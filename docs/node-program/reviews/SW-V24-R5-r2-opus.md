# SW-V24 R5 (EV-RC4) + R6 rdc side — review r2 (Opus)

| | |
|---|---|
| Target | rust-dashcore `node/ev-rc4`, range `15edfad82..9fa522cdd` (6 commits: R6 7877f108b, 9b2dadf86, 13b4ea222; r1 fixes 7acbddb1d, 2573f0ed8, 9fa522cdd) |
| Prior round | reviews/SW-V24-R5-r1-opus.md (ACCEPT with m1–m4, n1–n4) |
| Core | rc.4 `0e7382df80` (rc.3 `da567d7db` for contrast) |
| Base SDK fork | 029b40bda = merge of a2f04658, 231929ba, 94a518f6 on 569f4adfe (read only where rdc uses it) |
| Reviewer worktrees | /work/scratch/ev-rc4-opus-r2/{wt,wtm} (detached at 9fa522cdd, own targets, CARGO_BUILD_JOBS=12), plus a scratch base-sdk clone for revert mutants. All deleted after the review |
| Date | 2026-10-26 |

## Verdict

**ACCEPT.** I found no Blocker, Major or Minor; there are two doc-level Nits.

- Every r1 finding is fixed or explicitly dispositioned.
- The re-pin is consistent across all 13 workspaces.
- No rdc code still depends on rc.3's first-wins purpose read.
- Strict purpose reads give rc.4's verdict at every consumer I traced.
- When base-sdk's strict purpose read is reverted, seven tests in four suites fail. That includes the new dash-dml build-path test (m1) and the R6 regression input.
- The CE4-03 two-fault vector (m3) kills a reason-precedence mutant.

## Findings

### Blocker
None.

### Major
None.

### Minor
None.

### Nit

- **n1. The conformance renderer still models rc.3's purpose map.** `conformance/src/render.rs:422-440` (`NetInfo::groups`, outside this range).
  - Its doc says "first group wins on duplicates", and the body dedups and sorts.
  - Both of its sources are now strict decoders: `dc_ext_net_info` (dashcore, R5) and `bs_net_info` (base-sdk, R6). So every value it sees is already unique and ascending, and the branch is dead.
  - It does no harm, but it is the last rdc site that describes rc.3's behaviour as Core's.
  - **Fix:** reword the doc as "decoded values are unique and ascending (CE4-01)" and drop the dedup/sort. A `debug_assert!` of strict ascent could replace them.
- **n2. The `core_error` owner row is named imprecisely.**
  - BACKLOG BL-405 says "the dash-dml mnlist `core_error` override". The override is in `dash-oracle-sys/tests/mnlist.rs:64`.
  - The interim comment (`mnlist.rs:62-63`) and R5-notes N3 ("The manager assigns the re-transcription row") don't cite BL-405, even though the row now exists.
  - **Fix:** correct the crate in BL-405, and name BL-405 in the comment and in N3.

## Checks with no finding

### 1. r1 findings

| r1 | Disposition | Verified |
|---|---|---|
| m1 | `consumers::ev2b::net_info_purposes_read_as_rc4_reads_them` (7acbddb1d) | Covers v3 ProRegTx and ProUpServTx, with purposes [0,1,0], [0,0], [1,0] and [0,3] plus an ascending [0,1] control. It asserts both `payload(tx)` and the build's `bad-protx-payload`, and fails on the strict-revert mutant (§4). The ascending control's `payload(tx).is_some()` proves the hand-built layout decodes completely, so the refusals are the purpose rule and not a layout error. |
| m2 | Removed rather than marked (9b2dadf86); R5-notes N2 | `git grep` finds no `net_info_reads`, `ext_purposes_in_order` or `BASE_SDK_RC3_PURPOSES` anywhere in the tree, including node-fuzz and the nested workspaces. The node-fuzz model's `decode` now relies on `NetInfoV2::decode`. For refusals that is equivalent: base-sdk refuses exactly where the removed filter did, and both refuse a short or oversized count. Removing the public `netinfo::ext_purposes_in_order` is marked breaking (`refactor(dash-dml)!`); nothing in the repo used it. |
| m2 (`core_error`) | Kept, as an interim tied to BL-405 (N3, `mnlist.rs:62-63`) | Sound (see below). |
| m3 | "CE4-03 raw" row and the oracle pair (2573f0ed8) | Core gives `bad-protx-operator-reward` at reward 10001 and `bad-protx-netinfo-bad` at 10000, both asserted from the oracle, so the vector really carries two live faults. The check layer agrees at T12. It kills my mutant (§4). |
| m4 | BL-403 | Out of scope; I ran conformance without `bsdk-judges` as briefed. |
| n1 | R5-notes N1 (history not rewritten, `--no-ff` merge, bisect advice) | Acceptable. |
| n2 | node-program 8041b51b2 | ce.md CE4-02 now lists devnet among the networks that require routable addresses (chainparams rc.4:687). |
| n3 | `dash-oracle-sys/README.md` heading says ABI 29, and the text names the 27/28/29 bundles | OK. |
| n4 | R5-notes N5: CE4-18 goes to MN1, CE4-06's cache test to R19 | OK. |

**The `core_error` override.**
- rc.4's `ExtNetInfo::Unserialize` (netinfo.h rc.4:419-437) throws `ios_base::failure("ExtNetInfo: purpose unknown or out of order")` before the canonical-encoding check.
- rc.4's own `nested_mn_map_wire_order_is_canonical` (evo_snapshot_tests.cpp rc.4:1003-1021) now matches on `"ExtNetInfo:"`; rc.3 matched `"noncanonical MN object encoding"` at :1020.
- The records under `test-vectors/core/0364ed626ec2` were made at rc.3. The pinned oracle is rc.4. So overriding exactly the two cases, `netinfo_reversed_entries` and `netinfo_duplicate_entry`, with the rc.4 text is the only correct bridge.
- The `assert_eq!` on the rc.3 text trips on the first re-transcribed record of either case, as N3 claims.
- If a re-transcription renamed those cases, the override would go dead silently, but harmlessly: the record's own rc.4 error would then be used.
- `canonical_reads_match_core_tests` passes at the pin.

### 2. Re-pin
- `contrib/node/check-base-sdk-pins.sh` reports: "every package is built from file:///work/home/agent/workspace/base-sdk@029b40bda40e…". That covers the root workspace and the 12 nested workspaces (cs2-diff, dde-probe ×5, ev4-serve/evodump, the perfgate criterion shims ×2, spv-feature-off, node-fuzz).
- No manifest, lockfile or script still names 569f4adfe. The remaining mentions are history rows in BACKLOG, DECISIONS, STATUS and BS11a-3-notes.
- The lockfile diffs move only the base-sdk git source.
- `pure-clippy.toml`'s blst audit stamp moves to 029b40bda. That is justified: 569f4adfe..029b40bda touches only `pkgs/primitives` (netinfo.rs, the corpus JSON, and test code in transaction.rs), which cannot reach blst's pool.

### 3. Consumer-level parity of the strict purpose read

**rc.4's rule** (netinfo.h rc.4:422-437):
- The reader skips everything after an unknown version (0 or above 1).
- Otherwise it reads a CompactSize count, then for each group a purpose. It throws if the purpose is not in {0,1,2} or not above the last one, and only then reads the group.
- An empty group reads.

**base-sdk 231929ba** (`read_purpose`) does the same, returning `DecodeError::InvalidValue` at the purpose byte. Its extra count bound (`into_len(len/2)`) only refuses input that Core also refuses at EOF; it predates this range.

**Consumers:**

- **ProRegTx / ProUpServTx v3 on the build path.** `dash_dml::payload` (build/mod.rs:234) now requires only `is_complete()`. A strict-decode failure gives `None`, which `build_new_list_from_block` turns into `BAD_PROTX_PAYLOAD` (build/mod.rs:623). That is `TxValidationResult::Consensus` (reasons.rs:167), matching specialtxman.cpp's `GetTxPayload` failure → `bad-protx-payload` TX_CONSENSUS.
- **ProRegTx / ProUpServTx v3 on the check path.** node-l1 `dml_checks::validated` uses the same `payload`, so a failure lands at GVP 2, `bad-protx-payload`. `unknown_pro_tx_version` is `None` for version 3, so it cannot pre-empt.
  - The oracle-backed `extended_net_info_matches_cores_verdict` and the R6 regression `repeated_purpose_agrees_with_core` (BothReject) pass.
- **evodb records.** dash-dml's own `Reader::net_info` (codec.rs:491-528) was strict since R5 and is unchanged. Its test now asserts that base-sdk refuses with `InvalidValue`.
  - The mnlist oracle tests at rc.4 pass, with the message bridged above.
- **SML.** SPV reads through `dashcore` (strict since R5). dash-evo's `SmlEntry` only writes.
- **Other readers of ProTx payloads** (bloom, filters, conformance) use base-sdk's decode, which is now strict. That closes r1 §2's "R6's strict reader closes it" note.

**Port 443 (a2f04658).**
- rc.4 allows port 443 for a service only in the PLATFORM_HTTPS group (netinfo.cpp rc.4:59-66, 466-469), and for any domain (rc.4:482). I2P is checked first and must use port 0 (rc.4:461-464).
- This is not an rc.3→rc.4 delta: `git diff da567d7db 0e7382df80 -- src/evo/netinfo.cpp` has no hunk there. It is a base-sdk catch-up, and the fork's rstest rows match it, including i2p_https_port_443 → BadPort.
- **rdc needed no change.** dash-dml's validator already had the rule (`netinfo/entry.rs:63`; `netinfo/tests.rs:187-192` covers HTTPS 443 → Success, and Core P2P / Platform P2P 443 → BadPort). The only rdc paths that reach base-sdk's `Checkable` NetInfo check are:
  - conformance's best-chain `block.check`, which loses a would-be false alarm;
  - the test-only `ContextFreeNetInfo`.
- Consistent with that, reverting a2f04658 alone (mutant below) fails no test in dash-dml, conformance `--lib`, `l1_payload` or `l1_dml_block`. That is expected, not a gap: the fork owns the rule's tests.

### 4. Tests: reverts and hand mutants

| Mutant | Result |
|---|---|
| base-sdk with 231929ba and 94a518f6 reverted (rc.3 first-wins read), rdc unchanged | **Killed by 7 tests:** dash-dml `codec::records_read_purposes_strictly`, `netinfo::purposes_read_strictly`, `netinfo::validate_direct_statuses`, `consumers::ev2b::net_info_purposes_read_as_rc4_reads_them`; conformance `verbose::unreadable_payloads_flatten_like_core`; node-l1 `repeated_purpose_agrees_with_core`, `extended_net_info_matches_cores_verdict`. Every base-sdk-side tripwire claimed in N2 fires, and plain `cargo test -p dash-dml` sees it (r1 m1's ask). |
| base-sdk with a2f04658 reverted (no port 443 for HTTPS services) | Survives the rdc suites above, as expected (§3). |
| node-l1 check layer: rc.3's entry check (`check_net_info`) restored before T12 under the T12 label, env-gated (R2MUT) | **Killed** by `check_layer_mutant_inputs_compare_exactly`: the CE4-03 vector diverges with "check step reason". The unmutated build passes in the same binary. So the new vector pins the reason precedence and not only the step label. |

**Tautology check:**
- The new tests take their expectations from Core: the CE4-03 oracle pair, the oracle `BothReject` on the 328-byte R6 input, and rc.4's reject string.
- Or they assert a specific refusal: `DecodeError::InvalidValue` at the purpose rather than "any error". That distinguishes a purpose refusal from EOF or a bad entry.
- The rewritten `every_row_has_a_vector` row 205 is now a direct duplicate entry in one group, which still gives `bad-protx-dup-netinfo-entry` (providertx.cpp rc.4:205). The old vector needed `.with()` to undo base-sdk's first-wins read; it could not be built under the strict decoder.

### 5. Train-merge surprises
- None of the new tests needs the rc.4 gate environment. I ran everything without dashd or gate variables, and with default oracle discovery.
- `l1_payload` and `l1_dml_block` are behind `required-features = ["oracle"]`, as before.
- No `#[ignore]`, `should_panic` or skip gate was added.
- Expected at merge, not defects:
  - The re-pin changes the inputs of every PF0 bench crate, so it needs the usual bench-change approval (cf. DEC-424).
  - Any concurrent branch that pins another fork revision (BS11b) will conflict in the 14 manifest/lock files. Resolve by re-pinning to a fork merge that contains both.
  - node/base-sdk must keep 029b40bda reachable. The checker confirms it does locally.

### Test run
`cargo test --no-fail-fast -p dash-dml -p dash-evo -p dash-oracle-sys -p node-l1 -p dash-conformance --features dash-oracle-sys/oracle,node-l1/oracle` at 9fa522cdd (no `bsdk-judges`, no fuzzers, gates or PF0).
- The first invocation hit the harness's 30-minute background limit inside `l1_payments`.
- I reran the 11 node-l1 binaries it had not finished (`l1_payments` … `recorded_fixtures`) on their own.
- **Combined: 102 test binaries, 1224 passed, 0 failed, 18 ignored** (pre-existing quirk entries; the r1 count also included `dashcore`).
