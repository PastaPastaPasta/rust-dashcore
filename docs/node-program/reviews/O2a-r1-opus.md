# O2a review r1 (Opus): oracle shim v2, contextual entry points, COVERAGE.md

**Verdict: APPROVE-WITH-NITS.** No blockers, no majors. Before downstream L1 targets start using the bindings, fix findings 2 and 3 (the API hazard and the wrong doc wording). The matrix is accurate enough to plan from once the PLAN edits below are applied.

Under review: rust-dashcore `node/o2a-oracle-v2` at cde35d9f (15 commits on 5714489d), and the Dash Core branches `node/o2a-oracle-v2-v24.0` (c6c2645a42) and `-v23.1` (4868aa0cf7). At both pins, `oracle.cpp` and `oracle.h` are byte-identical (checked with `diff`); the `validation.cpp` trampolines differ only in v23's `nAdjustedTime` argument. Scope of this review: the coverage matrix as a planning artefact, the Rust bindings, the build, the evidence, and deviations 1–8. Adapter fidelity is GPT's.

## What I re-ran

| Check | Result |
|---|---|
| `cargo test -p dash-oracle-sys --features oracle` against the pinned archives in `/work/cache/oracle` (no `DASH_ORACLE_ALLOW_UNPINNED`), in a scratch worktree at cde35d9f | **57/57 pass**: archive_check 8, contexts 9, contextual 10, script 12, smoke 18 |
| Installed archive sha256 vs `pins.txt` (all 4) | equal; each MANIFEST names the commit, `wrapped _ZN4llmq5utils19GetAllQuorumMembers…`, and recipe sha256 `00b5a4a2…`, which equals the committed `build-oracle.sh` |
| Concurrency probe: `contextual` 8× at `--test-threads=32`, then `contextual` and `smoke` concurrently | 0 failures |
| **Fresh listdiff sample**: `dml-listdiff-check.py --count 20 --seed 20261009` against the mainnet oracle, read-only, one request at a time; the tip had reached 1,523,807, so the sample spans 1,072,142–1,521,183 | **20/20 match at v23.1 and at v24.0**. The sample included 1 ProRegTx, 2 non-null commitments and 1 punishing commitment. |
| The 7 punishing-commitment heights of the 1,000 run (1,099,043 … 1,315,499), re-run on the final pins | **7/7 match at both pins**, so PoSe through the member seam holds on the final archives |
| The 22 saved v23.1 samples from `dml-listdiff-1000-v23.1-service-samples.tgz`, re-run through `examples/dml_listdiff` on the final pins | **22/22 match at both pins** (see finding 1) |

Results are in `/work/scratch/o2a-opus/{ld-random20,ld-pose7}.jsonl` and `svc/svc-results.jsonl`.

## Findings

### 1. (minor) §22's "1,000 of 1,000 at both pins" needs its provenance stated

- **Where:** COVERAGE.md:319; `/work/scratch/o2a/evidence/dml-listdiff-1000.{jsonl,log}`.
- **Problem:** the recorded 1,000-sample jsonl says **v23.1: 978 match, 22 mismatch**. The run used the pre-review dev builds. The 22 v23.1 mismatches were turned into matches by a later harness normalization (a19fd5c0: v23.1 prints `service` as `[::]:0` when the net info is empty), not by a re-run. The final archives were checked only with a 100-block run.
- **Status:** my re-run closes the evidence gap. The 22 samples, the 7 PoSe heights and 20 fresh blocks all match on the pinned archives.
- **What §22 should say:**
  - The 1,000 run was on pre-fix dev builds, and the 22 v23.1 `service` records were reclassified.
  - The final pins were checked on 100 + 20 + 7 + 22 blocks.
  - The run used mainnet only, the legacy scheme, non-rotated quorums, and **`coins_have_best_block = 0`**. The example builds its context with `..Context::default()` (`examples/dml_listdiff.rs:92`), so RebuildListFromBlock's tip-only collateral re-check (`specialtxman.cpp` v24:378 / v23:255, `bad-protx-collateral`) never ran in the acceptance run. Only the DIP3-block unit test covers it.

### 2. (minor; fix before the first L1 consumer) `Context: Default` silently fills consensus-relevant fields

- **Where:** `dash-oracle-sys/src/context.rs:46` (`#[derive(Default)]`) and `:100`.
- **Problem:** with `..Context::default()`, a caller silently gets:
  - `bls_legacy = false`, the **basic** scheme. A pre-V19 context then decodes CbTx and QcTx BLS fields under the wrong scheme.
  - `deployments = []`. V24 and EVO_SHARES become DEFINED, so a regtest or testnet context silently runs pre-V24 rules.
  - `coins_have_best_block = false`, Core's Unchecked path.
  - `adjusted_time = 0`, meaning "unset". `SetMockTime(0)` then lets any time read fall back to the wall clock. Today only the header check reads the time, and it refuses 0.
  - `mn_list: Some(vec![])` and `None` serialize identically.
- **Failure scenario:** an EV1 or EV2 L1 harness that builds both sides from the same struct agrees with itself on the wrong era. The result is a loss of coverage that nothing reports. The acceptance example already ran under the Unchecked default without saying so (finding 1).
- **Fix:**
  - Drop `Default`.
  - Add `Context::new(first_height, headers, bls_legacy, adjusted_time)`.
  - Model the coins as `enum CoinsView { Unchecked(Vec<ContextCoin>), AtTip(Vec<ContextCoin>) }`.
  - Make the deployments explicit, or take them from a `Chain` profile helper.
  - Document that empty `mn_list` bytes mean "no list".
- **Other notes on the same type:**
  - `QuorumMembers.members` is concatenated with no per-member length. A truncated member can shift its neighbours' bytes. The shim usually refuses that through count or trailing-byte checks, but not always.
  - Version-bits seeds apply only "for the block after the tip". A future entry point that asks `DeploymentActiveAt(*tip)` across a period boundary would silently see DEFINED. Record this as a constraint for O2a-EV and O2a-LQ1. No current adapter does it: I checked every V24/EVO_SHARES query site at both pins, and the only `ActiveChain()` reads are on RPC or ProcessSpecialTxsInBlock paths that no adapter reaches.

### 3. (minor) `oracle_check_special_tx` refuses every MNHF signal and asset unlock, not only those that reach the quorum manager

- **Where:** `oracle.cpp:1840-1846`; `oracle.h:399`; COVERAGE E4/E6 (lines 138, 140).
- **Problem:** the guard returns ORACLE_E_CONTEXT for type 7 or 9 whenever DIP0003 is active. Core's `CheckMNHFTxImpl` decides several cases before `get_quorum`: `bad-mnhf-payload`, IsTriviallyValid, `bad-mnhf-non-ehf`, `bad-mnhf-quorum-hash`. `CheckAssetUnlockTx` likewise checks version, payload, fee and window first. Those reject strings (O17 EVO-02 and MPIX-16 rows) therefore have **no L1 at all**. oracle.h:399 says "when Core would reach their checks", which is not what the code does.
- **Fix:**
  - Now: correct the wording in oracle.h and COVERAGE E4/E6 ("refused for every type 7/9 once DIP0003 is active").
  - The coverage itself comes with the O2a-LQ1 extension, below.
  - Do not replicate Core's early checks in the shim, which would be adapter logic (M-7).

### 4. (minor) The error mapping is exact but untyped and silently truncating

- **Where:** `oracle.h:78-82`; `oracle.cpp:276-282` (WriteState); `lib.rs:373-384`.
- **What holds:** class, reason and debug message come straight from `GetResult`/`GetRejectReason`/`GetDebugMessage`. A seam miss overrides any verdict Core built from the caught exception (`WithContext`, `oracle.cpp:1139-1162`).
- **Gaps:**
  - (a) `reject_reason[128]` and `debug_message[512]` are truncated with no flag. A long LLMQ log line, which `verify_commitment` returns as `debug_message`, would show up as a false divergence.
  - (b) `Rejection.result` is a bare `i32`. TxValidationResult and BlockValidationResult share the integer space, and `verify_commitment` returns 0. A consumer can compare across enums by accident.
  - (c) `from_utf8_lossy` can map distinct non-UTF-8 messages to equal strings.
- **Fix:**
  - Have the Rust side treat `len == cap-1` as truncated (an `ErrorKind` or flag), or add the full length to a future ABI.
  - Add a `ResultKind { Tx, Block, Bool }` to `Rejection`, set per entry point.
  - Keep raw bytes alongside the lossy string.

### 5. (minor) `payload_decode` restores the BLS flag only on `std::exception`

- **Where:** `oracle.cpp:2110-2121`.
- **Problem:** a non-`std::` throw would escape to `Guarded` with `bls_legacy_scheme` left flipped for the rest of the process. ChainContext already uses RAII.
- **Fix:** use a small scope guard.
- **Context:** this is the only manual save/restore in the file. Every `extern "C"` entry point is wrapped in `Guarded`/`WithNetwork`/`WithContext`, which end in `catch (...)`, so no exception crosses the FFI. There are no Rust callbacks. Buffers are caller-owned, and `with_buffer` retries exactly once with the size the shim reports (`WriteBuffer` sets `*out_len` before returning E_BUFFER). That is sound for the deterministic entry points.

### 6. (minor) Smoke coverage is thin for three entry points

- **Where:** `tests/contextual.rs:173-214` and `:136-171`.
- **Problem:**
  - `sequence_locks` is exercised only with a version-1 transaction (Core's early-out, `(-1,-1)`) and the refusal paths. `CalculateSequenceLocks`' BIP68 arithmetic and the MTP path are never run.
  - `contextual_check_tx` and `contextual_check_block` have accept cases only. `contextual_check_header` adds just `time-too-new`.
- **Fix:** before CS3 and CS5b start their L1 targets, add Core-derived rejections:
  - a version-2 transaction with height and time relative locks: one satisfied, one not, with expected values from Core's `miner_tests`/`txvalidationcache_tests` cases or a regtest `feature_bip68_sequence` capture;
  - `bad-cb-height`;
  - `bad-txns-nonfinal`;
  - `bad-txns-type` (CONS-06, per pin).

### 7. (minor) Build: the wrap list comes from `__real_` references only

- **Where:** `build-oracle.sh:231`, `:240`.
- **Problem:** a future `__wrap_X` with no `__real_X` reference (a pure replacement) would get no `--wrap` and stay silently inactive. The `nm -u … | head -5 || true` check also swallows an `nm` failure.
- **Fix:** collect both the `__real_` references and the defined `__wrap_` symbols of `libdash_oracle.a`, and fail unless the two sets are equal. Run `nm` without `|| true`, filtering in awk so SIGPIPE can't occur.
- **What already holds:** the seam is proven effective at runtime at both pins. A context without members gives ORACLE_E_CONTEXT, and the punishing commitment verifies against context members. `--wrap` cannot reach llmq/utils.cpp's own internal calls, as the shim comment says. No adapter reaches those calls. I checked `RebuildListFromBlock`, `CheckLLMQCommitment`/`Verify` and `HandleQuorumCommitment` at both pins: all of them call `GetAllQuorumMembers` across object files.

### 8. (minor) Use a depends cache per pin, as the implementer proposes

- **Where:** `build-oracle.sh:205-207`.
- **Why it's safe:** `BASE_CACHE="${cache}/depends-cache"` is shared by every pin in a cache root. Because of `locks/depends.lock` this causes eviction, not corruption. The archive bytes don't depend on the cache path; the script's "object contains `${cache}`" check would catch a leak.
- **Concrete change:**
  - `depends_id="$(git -C "${dash_src}" rev-parse "${commit}:depends")"`; for `--dev`, `git -C "${dev_src}" rev-parse HEAD:depends`, falling back to the pin name if `depends/` is dirty.
  - `BASE_CACHE="${cache}/depends-cache/${depends_id}"`.
  - Lock `locks/depends-${depends_id}.lock`.
  - Keep the shared `SOURCES_PATH` and its download lock.
  - Record `depends_cache ${depends_id}` in the MANIFEST.
- **Impact:** existing pins need no rebuild. Do it with this PR or before the next pin move. It also stops dev builds evicting other agents' entries in `/work/cache/oracle`.

### 9. (nit) `pins.txt` names the O2a branches, but the README still names `node/oracle-shim-*`

- **Where:** `pins.txt:10-11`; `README.md:9-10`.
- **What I verified:**
  - Both `node/oracle-shim-*` are ancestors of the O2a heads, so a fast-forward is possible. They are checked out in `dash-wt/oracle-shim-*`, so fast-forward there with `merge --ff-only`.
  - `cached_archive_ok` checks pin, commit, flavor and symbol prefix, but not the branch.
- **Fix:** after the fast-forward, change column 3 back to `node/oracle-shim-*` at merge. Commit and sha stay as they are, and the cached archives stay valid. The MANIFEST's `branch` line is informational.

### 10. (minor) COVERAGE.md issues

| # | Where | Issue | Fix |
|---|---|---|---|
| a | §4 rows C1–C6 vs §21.3 C1–C11; §15 rows L1–L3 vs layers L0–L4 | The same IDs mean different things | Rename the contradictions X1–X11 and the ChainLock rows CL1–CL3 before other documents cite them |
| b | G1 (line 209); §20 `superblocks_gate`/`old_budget_check` | G1 says "(O2a, **not written**)" | Change to **O2a-GV**, consistent with Y5 and N21 |
| c | Missing row | Sporks as consensus inputs. SPORK_19 gates ChainLock enforcement (`chainlock.cpp` v23:18 / v24:33, so chain selection, LK1a, M4); SPORK_3 gates IS block filtering (v23 `instantsend.cpp:823`, the L3 `is_gate` row); SPORK_9 gates superblocks (v23 `governance.cpp:1507`, EV3 at M4) | Add **row V5** to §14: Rust `SporkView` (SP0); Core CSporkManager::IsSporkActive/GetSporkValue; oracle none (an input to O2a-GV's context); L2 spork values recorded per block (O8 decisions); L3/L4 FT `feature_sporks`, `p2p_sporks` per profile; notes O17 spork rows and the v23 multi-key model |
| d | M1 (line 147) and §21.3 C2 | "Item 1 absorbed" is overstated. EV2 §6.3 item 1 asks for the full result **and canonical list bytes**. `oracle_dml_apply` returns the SER_DISK `CDeterministicMNList` (CHAMP order of a **reloaded** list, since Core's Unserialize re-adds entries: DEC-54's reload order, not the online-sync order) | Say so in M1; O2a-EV adds canonical output (v24) or relies on `oracle_mnlist_canonical(bytes)`. Bucket-order comparisons go through `oracle_mnlist_core_order(ops)` |
| e | E4, E6, N7 | Refusal scope (finding 3) | Reword as in finding 3 |
| f | M1, §22 | Missing caveats | Add those from finding 1 |
| g | Header line 3 | Says "DECISIONS up to DEC-58" | The matrix uses LQ1 rev4, so it should say DEC-60 |
| h | N13 | The proposal is "O2a or BS12" | This is the only gap that blocks an **M1** target (`diff_bls_*` of BS11b; BS12's "dashbls via the oracle"). It needs an owner now, not with the other follow-ups (PLAN edit 9) |

The rest is accurate.
- **Row count:** 89 matrix rows (S7 D6 C6 P5 Y5 U4 T5 E7 M10 Q8 B4 K3 V4 L3 G2 H8 I2).
- **Spot-checked claims, all true:**
  - C1: `oracle_transaction_sigops` is O1b's, at `oracle.cpp:1727`.
  - C6: 12 special types at v24 and 9 at v23.1 (`primitives/transaction.h`).
  - C10: A1 App. D:2920 has `feature_dip4_coinbasemerkleroots` "(mixed A)" at M4, while §4.8:2434 puts mixed A at M5.
  - DEC-50(3) says "seven", while EV2 §6.3 lists eight.
  - The C5 refusal rule is the 11-header MTP rule as coded.
- **Rulings proposed for §21.1:**
  - Accept N1, N2, N3, N5, N18, N19, N20.
  - N4 → CH6a, as an `oracle_coin_codec` op.
  - N7, N10, N11, N12, N22 → the O2a-LQ1 extension.
  - N8, N9 → O2a-EV.
  - N13 → BS12, now.
  - N14 → O2a-GV, since EV3 is the single consumer.
  - N15 → a CS4-owned shim follow-up in the O1b pattern (DEC-25(3)); the context's seeding code is most of it.
  - N16 → IX1.
  - N17 → the PLAN edits below.
  - N21 → O2a-GV.

### 11. (note, positive) Process state and threading are handled correctly

- **Shim side:**
  - Contextual calls hold the shim's `ProcessStateMutex` and `cs_main`.
  - ChainContext saves and restores the BLS flag, mock time and seam pointer.
  - The seam state is `thread_local`, and `g_missing` is reset at each construction.
  - Placeholder ancestors are shared and immutable.
  - Objects the special-tx processor references but the adapters never reach sit on a PROT_NONE page, so a wrong path faults loudly instead of reading garbage.
  - `LogCapture` stores lines through a `thread_local` pointer, so concurrent calls on other threads cannot leak into `debug_message`.
- **Rust side:** `on_chain` holds the per-pin network mutex for the whole call, so `set_network` cannot race a contextual call.
- **One caveat:** the oracle's own block index (Core's `CBlockIndex` map) holds only the context's headers. Any future entry point that touches `ActiveChain()` will see an empty chain. That is a constraint for O2a-EV and O2a-LQ1, not a bug today.

## Deviations 1–8

| # | Deviation | Acceptable for merge? |
|---|---|---|
| 1 | `oracle_block_value_payee` deferred to O2a-GV | **Yes.** v23.1.8's payments processor needs CGovernanceManager, CMasternodeSync and CSporkManager, which takes its own context design. The consumer, EV3, is M3–M4. Conditions: O2a-GV gets a PLAN row (below), and EV3's acceptance points at it |
| 2 | `check_special_tx` runs the mempool path | **Yes.** For types 1–6, 8 and 10–12, `CheckSpecialTxInner` ignores `chain` and `indexes` (v24 specialtxman.cpp:217-274), so the mempool path is the block-path code. Types 7 and 9 move to O2a-LQ1, with the doc fix from finding 3 and the EV1 wording edit |
| 3 | Added `oracle_payload_decode` | **Yes, and welcome.** It is in A1's v2 list and is what BS4b and BS6b need. Requires the BS4b dependency edit |
| 4 | `dml_apply` absorbs EV2's "O2a-EV item 1" | **Yes, with a correction.** The member seam, best-block flag, ancestry and V24 flag are there, but the output is not canonical bytes and the input list is in reload order (finding 10d). EV2 §6.3 needs an erratum |
| 5 | No `dos` field | **Yes.** Neither pin has a DoS score; the penalty derives from the result class in net_processing. Needs an A1 §4.4 erratum. `verify_commitment`'s debug message is pin-specific log text, so compare it per pin and never across pins |
| 6 | Broad refusals | **Yes, for a raw helper.** EV2 §6.3 already restricts `diff_dml_apply` to the reachable intersection, so the harness must count ORACLE_E_ARG as out of domain, never as agreement or divergence. Condition for O2a-EV: `oracle_dml_block` runs Core's check loop **before** applying any raw-helper precondition, so validated-block verdicts are never pre-empted |
| 7 | `pins.txt` names the O2a branches | **Yes.** At merge, fast-forward and change the column back (finding 9) |
| 8 | Acceptance range is pre-V19 mainnet | **Yes for merge.** The PLAN criterion, "1,000 sampled mainnet blocks", is met literally, and my sample extends it to 1.52M. Evonodes, the basic scheme and rotated quorums are unverified. Make a post-V19 re-run (members from Core via `oracle_llmq_members`) part of O2a-LQ1's acceptance. Add a testnet sample for the v24 pin, which is the testnet reference (DEC-05) but was only exercised on mainnet data |

## Proposed PLAN edits

1. **CS5b** (§4.2):
   - Deps `CS3` → `CS3, O2a`.
   - In the acceptance, replace "L1 `oracle_check_tx_inputs`, `oracle_sequence_locks`, `oracle_sigops` (O2a)" with "L1 `diff_check_tx_inputs`, `diff_sequence_locks` (both std, per pin) through `oracle_check_tx_inputs`, `oracle_sequence_locks` (O2a), and `diff_sigops` through `oracle_transaction_sigops` (O1b)".
   - Keep "MuHash and `hash_serialized_2` equal the oracle at sampled heights". That oracle is the RPC (`gettxoutsetinfo`); there is no shim entry (COVERAGE U3/N5, accepted).
2. **BS4b** (§4.1):
   - Deps `BS4a, O1, O3` → `BS4a, O1, O2a, O3`.
   - Task text: replace "differential decode targets vs the oracle for `Transaction`, `Block`, `BlockHeader` and every payload" with "differential decode targets vs the oracle for `Transaction`, `Block`, `BlockHeader` (`oracle_{tx,block,header}_decode`, O1) and every payload type 1–12 × version under both BLS flags (`oracle_payload_decode`, O2a; types 10–12 on the v24 shim only)".
3. **EV1** (§4.5):
   - Task: "(13 types × versions)" → "(12 types × versions; 10–12 v24 only)".
   - Deps `EV0, CS3, BS6a, i:BS6c` → `EV0, CS3, BS6a, O2a, i:BS6c, i:O2a-LQ1`.
   - Acceptance: replace "L1 `oracle_check_special_tx` (O2a: full `CheckSpecialTxInner` with synthetic list/coins/ancestry/deployments/BlsCtx; v1's `oracle_dml_apply` did not cover this, F15)" with "L1 `diff_check_special_tx` (std, per pin, under each profile's rules) through `oracle_check_special_tx` (O2a: `CheckSpecialTxInner` on the mempool path, with no chain and no asset-unlock index set, against a synthetic list, coins, ancestry, deployments and BLS flag; for types 1–6, 8 and 10–12 this is the block-path code). MNHF (7), asset unlocks (9) and the block-path chain/index-set variants go through O2a-LQ1's quorum-manager context; until it lands they are L2/L3 only".
4. **New row O2a-EV** (§4.7, after O2a):
   - **Task:** Oracle follow-up for the DML (EV2 §6.3 items 1–2, DEC-50(3)):
     - `oracle_dml_block`: the validated-block layer. Core's block-path `CheckSpecialTxInner` loop, then `RebuildListFromBlock`, then `CDeterministicMNManager::ProcessBlock`, profile-selected. It returns the rejecting stage, Core's state and canonical list bytes. No raw-helper refusal is applied before Core's check loop. It includes v24's shared-collateral template check (N8).
     - The eight list entry points: `oracle_mnlist_canonical`, `oracle_mnlist_diff`, `oracle_mnlist_apply_diff`, `oracle_state_diff`, `oracle_mnlist_core_order(ops)`, `oracle_mnlist_payee`, `oracle_mnlist_projected` (with deployment context) and `oracle_unique_property_hash`.
     - Canonical-bytes output for `oracle_dml_apply` (v24 shim).
     - `oracle_sml_entry_hash` and the `merkleRootMNList` calculation (N9).
     - `oracle_special_tx_sign_hash`, for BS6b's sighash vectors (T3).
   - **MS** M2–M3, before EV2a-3 (codec and hash targets) and EV2b-2 (block targets). **Crates/repo** Core `node/oracle-shim-*` branches; dash-oracle-sys. **Deps** O2a. **Size** M. **Risk** CON (adapter logic, M-7). **Design** — (EV2 §6.3). **Model** O-xh. **PRD** NFR-CEQ-04.
   - **Acceptance:**
     - a smoke differential test per entry point on both pins (codec and order entry points on the v24 shim only);
     - `oracle_dml_block` equals `protx listdiff` on ≥ 200 sampled mainnet blocks and returns Core's reject string and stage for one crafted block per EV2 §6.5 operator;
     - `oracle_mnlist_canonical` hash equals O8 `getcanonicalmnlist` at ≥ 20 heights;
     - committed `oracle_unique_property_hash` vectors;
     - reproducible pins.
   - Also add `O2a-EV` to EV2a's deps (as `i:` for EV2a-3) and to EV2b's deps.
5. **New row O2a-LQ1** (§4.7):
   - **Task:** the LQ1 §6.2 oracle context: `oracle_llmq_ctx_new/free`, `add_header`, `set_mn_list`, `set_coinbase`, `set_snapshot`, `set_mined`, `type_enabled`, `members`, `precompute`, `scan`, `select`, `restart` and `rows_digest`; the DEC-16 record-only trace hooks with the trace format; the canonical `llmq_S` row digest; and the fixture loader for LQ1a-A's Core bench. Inside these contexts O2a's `GetAllQuorumMembers` wrap stays off, so Core's own function, caches and hooks run.
   - **Extension**, in the same task, second phase, before M4 (N7, N10, N11, N12, N22). Using the context's CQuorumManager:
     - `oracle_check_special_tx_block` (chain overload and asset-unlock index set), covering MNHF (7) and asset unlock (9);
     - the in-block commitment rules (`CQuorumBlockProcessor::ProcessBlock`, incl. O17 DKG-07);
     - `oracle_quorums_root`;
     - `oracle_sign_hash` and `oracle_request_id`;
     - the CbTx best-CL check.
   - **MS** M3 (phase 1, before LQ1a-A merges) → M4 (extension). **Crates/repo** Core shim branches; dash-oracle-sys. **Deps** O2a. **Size** L. **Risk** LLMQ/CON. **Design** — (LQ1 §6.2, §8 Q2). **Model** O-xh. **PRD** NFR-CEQ-04, FR-LLMQ-01/02.
   - **Acceptance:**
     - the LQ1 §6.2 digest known-answer vectors;
     - the hooks change no control flow: reviewed, and Core's `llmq_snapshot_tests`/`llmq_utils_tests` pass with hooks on;
     - members equal `quorum info` recorded on an O7-style replica for every mined quorum in sampled windows, including mainnet's first V20 cycle (1,737,792) and testnet;
     - **the `oracle_dml_apply` listdiff check re-run on ≥ 1,000 mainnet blocks from 1,899,072 to the tip**, with members from `oracle_llmq_members` (closes O2a deviation 8), plus ≥ 200 testnet blocks on the v24 pin;
     - MNHF and asset-unlock verdicts equal Core's on mined transactions and on one crafted reject per early check (finding 3).
   - Add `O2a-LQ1` to LQ1a's deps (LQ1 §7) and to LQ2's deps.
6. **New row O2a-GV** (§4.7):
   - **Task:** the EV3 oracle prerequisites:
     - `oracle_block_value_payee`: `IsBlockValueValid`, `IsBlockPayeeValid` and `GetBlockTxOuts`, per pin, with a governance context. The context holds the sync flag (the F3/F6 leniencies), spork values (v23.1 CSporkManager with SPORK_9; v24's SuperblockManager inputs), superblock triggers as payment lists, the old-budget gate (MNO-12), the tip's MN list and the platform share.
     - `oracle_credit_pool_step` (N14).
   - **MS** M3–M4, before EV3's gate. **Crates/repo** Core shim branches; dash-oracle-sys. **Deps** O2a (and SP0's spork-value semantics, as an `i:` dependency). **Size** M. **Risk** CON. **Design** a short context-format note reviewed with EV3's brief. **Model** O-xh. **PRD** NFR-CEQ-04, FR-CON-21, -22, -27.
   - **Acceptance:**
     - verdicts and expected outputs equal the mined coinbases at sampled heights, including the superblock window from 614,820 and every 576th block from DIP3, at both pins;
     - one crafted reject per Core reject string (`bad-cb-amount`, `bad-cb-payee`, the superblock strings);
     - the not-synced leniency per pin;
     - v24 `strict_multiplicity` with V24 on;
     - `oracle_credit_pool_step` equals `getcreditpoolinfo` from 1,987,776 (the v24 oracle).
   - **EV3:** replace "`oracle_block_value_payee` (O2a)" with "`oracle_block_value_payee`, `oracle_credit_pool_step` (O2a-GV)", and add `O2a-GV` to its deps.
7. **O2a row** (§4.7):
   - Replace "block value/payee" in the entry-point list with "`oracle_payload_decode`; block value/payee moved to O2a-GV".
   - Replace "full result classes" with "result class, reason and debug message (no `dos`: neither pin has one; A1 §4.4 erratum)".
8. **CS3** (§4.2, C11/N17):
   - Deps `CS0, BS4a` → `CS0, BS4a, O2a`.
   - Replace "`oracle_contextual_check_*` (O2a)" with "`diff_contextual_check_tx`, `diff_contextual_check_block_header` (CS2 §6.2's name), `diff_contextual_check_block` (std, per pin) through O2a's `oracle_contextual_check_*`".
9. **BS12 and BS11b** (N13): add to BS12's task "the multi-op `oracle_bls` shim entry points (aggregate, secure aggregate, threshold split/recover, IES; A1 §4.4), as a DEC-16 follow-up on the O2a shim branches", and change BS11b's L1 note to depend on it.
10. **CS4** (N15): add "`oracle_versionbits_state` on the O2a shim branches, built from the O2a context's seeding code (an O1b-style follow-up, DEC-25(3))" to CS4's task and deps.
11. **Wording sweep:**
    - BS6b "13 types" → "12 types (1–12; 10–12 v24 only)".
    - Apply DEC-50(7) to EV2a/EV2b (C9).
    - A1 errata: §4.4 `dos` (C5), "13 types" (C6), the O2a v2 list (C8), App. D mixed-A placement (C10).
    - EV2 §6.3 erratum for item 1 (finding 10d).
    - Give O1c its PLAN row (DEC-48 Q1), which is still missing.

## Summary

The shim and bindings are well built:
- every FFI entry point is exception-sealed;
- process-wide state is serialized and restored;
- the `ld --wrap` seam is real, needs no Core source patch, and is proven effective at runtime at both pins;
- the archives are reproducible and pinned;
- 57/57 tests pass on the pinned archives.

My independent read-only re-run is 49 of 49 matches at both pins: 20 fresh blocks up to 1.52M, the 7 PoSe heights and the 22 previously reclassified v23.1 samples. That confirms the listdiff claim on the final archives.

Nothing blocks merge. Fix first:
- the `Context::default()` hazard (finding 2), before any L1 harness adopts the API;
- the MNHF/asset-unlock refusal wording (finding 3);
- §22's provenance and caveats (finding 1).

The matrix is a sound planning artefact. Its gaps list and contradictions are accurate. It misses one row (sporks as consensus inputs), and its "item 1 absorbed" claim needs qualifying. With PLAN edits 1–11, every M1–M4 "none" has an owner or an accepted reason. The only gap that is urgent for M1 is N13, multi-op BLS, which edit 9 assigns to BS12.
