# EV1 special-transaction design, round-1 Sol review

Target: `node/ev1-design` at `1490556af`; reference: Dash Core `v24.0.0-rc.3`. This is a design review; no code was changed or built. I checked the dispatcher and reject census, all table rows for types 1–4 and 12, and the rows for types 5–11, NetInfo, and the covenant against the pinned source. The listed line references are at that pin.

## Major

### M1 — D9 still assigns migration code and calls to EV1

**Doc:** §0 item 6, §1.2, §2.6, §3.4 step 7, §3.5 step 5b, §7 EV1-3/EV1-4, §8.1 D9. **Core:** `src/evo/specialtxman.cpp:51-137,1363-1367,1471-1476`.

The settled D9 ruling puts `dash-dml::migrate` (`set_state_version` and `apply_pro_up_serv`) wholly in EV2b, with no EV1 calls into it. The design still says the first of EV1-3 and EV2b to land writes it, and assigns both functions to EV1 slices. That cannot be the implementation contract. At the same time, Core's type-2 and type-3 *contextual checks* do run a scratch migration before later uniqueness, collateral, and signature checks and forward `bad-protx-netinfo-version` as `TX_BAD_SPECIAL` on failure. EV1 therefore still needs an EV1-owned, read-only equivalent of the failure predicate and ordering, based on its `add_entry` primitive, without calling or owning EV2b's mutation functions. Specify that seam and its equality tests; remove the shared ownership and slice dependency cycle.

### M2 — Legacy BLS decode is treated as a test-generator limit, although early EV1 slices use it in validation

**Doc:** §3.3 steps 6–7, §3.5 steps 3–4, §6.1, §7 EV1-1/EV1-3, §8.1 D10. **Core:** `src/evo/providertx.cpp:300-304,468-472`; `src/bls/bls.h:100-123,183-208,409-559`.

Core calls `CBLSLazyPublicKey::Get().IsValid()` for version-1 ProReg and ProUpReg payloads. The BS11a-t prerequisite supplies basic wrapper types, while BS11a supplies exact legacy decompression. D10 only restricts L1-generated legacy points until BS11a lands, yet EV1-1 promises complete trivial validation and EV1-3 begins checking types 1 and 3. A valid Core legacy point outside the BS11a-t approximation can then get a different `bad-protx-key-null` verdict, or a malformed point can take a different path. Constraining a fuzzer does not constrain received transactions. Gate production use of these checks and the EV1-3 merge on exact BS11a legacy decode, or specify a separate Core-equivalent decoder available before that slice. Keep the L1 corpus restriction only as a temporary measurement limit, not as the parity claim.

### M3 — The proposed NetInfo address buffer cannot hold a valid domain

**Doc:** §3.16, §5.1 item 1, §8.1 D4. **Core:** `src/evo/netinfo.h:22-25,187-188`; `src/evo/netinfo.cpp:90-113,401-418,480-498`.

`DomainPort` accepts domains up to 253 ASCII bytes, and `HasAddrDuplicates` compares complete `ToStringAddr()` strings. The proposed `ArrayString<64>` can neither represent a valid long domain nor compare two such domains exactly. Truncation can create a false `Duplicate`; failed formatting or a panic can turn Core's success or later reject into a fault. Use a nontruncating borrowed comparison or a capacity covering the full 253 byte domain and the service-address forms. Add two distinct domains sharing their first 64 bytes and a duplicate long domain to the NetInfo vectors.

### M4 — The quorum scan buffer is one slot short for a permitted devnet configuration

**Doc:** §2.3 `QuorumLookup::scan_quorums`, §3.12 type 9 step 10, §5.1 item 1, §8.1 D1. **Core:** `src/evo/assetlocktx.cpp:113-126`; `src/llmq/params.h:380-398`; `src/chainparams.cpp:1308-1327`.

Asset-unlock verification requests `signingActiveQuorumCount + 1` quorums. The design fixes the output at `ArrayVec<QuorumHash, 32>` and asserts that the requested count is at most 32. Core's `llmq_60_75` has 32 active quorums, and the `-llmqplatform` devnet argument may select it, making the request 33. Once 33 quorums are returned, the seam must lose one, fail, or overflow, potentially changing `bad-assetunlock-too-old-quorum`. Size the buffer from the maximum selectable Platform LLMQ count plus one (at least 33 at this pin), or make the capacity parameter driven by the selected LLMQ parameters, with an explicit no-truncation invariant and a devnet vector.

### M5 — The mempool needs two distinct coins views, but its binding does not name either

**Doc:** §2.3 `coins`, §2.6 MP1a, §3.17 call-site table, §8.1 D1. **Core:** `src/validation.cpp:802-810,999-1018,1097-1104`; `src/evo/specialtxman.cpp:1186-1199,1488-1496,1815-1828`.

Core binds `CheckSpecialTx` to `m_active_chainstate.CoinsTip()` even when the mempool's input cache contains unconfirmed outputs, so an external ProReg collateral created by an unconfirmed transaction is absent to EV1. The mempool covenant instead uses `m_view` after `CheckTxInputs`, which has cached the transaction's inputs from the mempool-backed view. The design says only “the caller's view” for `SpecialTxCtx.coins` and does not state the view passed to `shared_collateral_spends` on MP1a. A single mempool-inclusive view for both calls changes the ProReg verdict; a tip-only view for both can miss a shared-template spend of an unconfirmed output. Normatively bind `ctx.coins = CoinsTip` for the special check and pass the mempool input cache to the separate covenant call. Test both cases.

### M6 — NetInfo validation must interleave purpose checks with that purpose's entries

**Doc:** §3.16 numbered steps 3–4, §6.1 NetInfo order. **Core:** `src/evo/netinfo.cpp:588-637`; `src/evo/providertx.cpp:187-210`.

The design presents all per-purpose prechecks as step 3 and all per-entry validation as step 4. Core instead checks a purpose's validity, size and address duplicates, then all of its entries, before advancing to the next purpose. For example, a structurally valid but unroutable service in purpose 0 and five distinct entries in purpose 1 return `NotRoutable` in Core on a network requiring routability; a two-pass implementation would return `MaxLimit` from purpose 1 first, giving another `bad-protx-*` reject. Make the nested order explicit in the normative table and add a mixed-error vector across two purposes.

### M7 — The NetInfo `AddEntry` fuzz oracle cannot be the ProUpServ wrapper

**Doc:** §6.5 `netinfo_validate`/`netinfo_add_entry`, §8.2 A-3. **Core:** `src/evo/netinfo.cpp:307-349,420-450,500-586`; `src/evo/specialtxman.cpp:51-137,217-274,1363-1367`.

`oracle_check_special_tx` returns a transaction verdict. Its ProUpServ path does not call `AddEntry` on arbitrary supplied text; it only reaches `AddEntry` indirectly while migrating state, using existing entries' `ToStringAddrPort()`, and it returns neither the `NetInfoStatus` nor the resulting entry. Thus the proposed `netinfo_add_entry` target cannot assert “status and resulting entry equal” through that wrapper. The `netinfo_validate` wrapper also cannot expose statuses preempted by ProUpServ's trivial checks. Make A-3 a prerequisite for those differential fuzz properties, or narrow the targets to properties the current oracle actually observes and add direct Core vectors for the inaccessible statuses.

### M8 — Types 7 and 9 lack a Core-equivalent negative context gate until A-1 lands

**Doc:** §6.1–§6.3, §7 EV1-5, §8.2 A-1. **Core:** `src/evo/mnhftx.cpp:107-153`; `src/evo/assetlocktx.cpp:106-153,177-232`.

The L0 census promises one vector per reject, but it can only test EV1 against hand-constructed expected values. L2 history exercises successful transactions. At the pin, `feature_mnehf.py` largely exercises successful signals and in-block duplicate handling, while `feature_asset_locks.py` covers selected signature, recency, expiry and duplicate-index rejects. They do not independently exercise all of the type-7 and type-9 contextual branch order, quorum lookup variants, or combinations where two rejects are possible. The EV1-5 checkpoint can therefore pass while its quorum-context semantics disagree with Core. Add an explicit L3 negative matrix against the pinned node for each reachable type-7/9 context branch and first-reject pair, or make A-1's block oracle a merge prerequisite for EV1-5. Retain L0 census closure as a separate check.

## Minor

### m1 — Reject census generation must enumerate nonliteral `Invalid` calls, not only literal matches

**Doc:** §3.13, §7 EV1-1. **Core:** `src/evo/specialtxman.cpp:1367,1476`.

At this pin the stated 213 literal sites, 144 `TX_BAD_SPECIAL`, 69 `TX_CONSENSUS` and 146 distinct literal strings match the scoped source; the two migration sites forward `migration_state.GetRejectReason()`. A script that only greps `Invalid(..., "...")` cannot detect a newly added variable-built reason. Have the census enumerate every `Invalid` call in the scoped functions and require every nonliteral argument to be classified explicitly. The two current forwarding sites are the initial entries.

### m2 — State the exact scope of the type-7/9 `check_sigs` exception in the test matrix

**Doc:** §3.12, §6.1–§6.3, §8.1 D2. **Core:** `src/evo/mnhftx.cpp:151-153`; `src/evo/assetlocktx.cpp:147-152`; `src/evo/specialtxman.cpp:871`.

The tables correctly leave MNHF and asset-unlock `VerifyInsecure` unconditional, even when CH7a passes `check_sigs = false` under assumevalid. Add explicit false-`check_sigs` vectors for both types in the EV1-5 checkpoint; otherwise the broad “check_sigs true and false” generator covers only types that the current oracle reaches. This is a test specification change, not a table correction.

## Checks that matched the pin

- The type-1 through type-4 and type-12 trivial and contextual tables preserve the source order, reject class and literal string at the checked sites, including the forwarded migration reason's `TX_BAD_SPECIAL` class.
- The dispatcher reproduces the DIP3 gate, coinbase special cases, `std::exception` mapping to `TX_CONSENSUS` `failed-check-special-tx`, and the unhandled-type `TX_BAD_SPECIAL` reject (`src/evo/specialtxman.cpp:217-274`). The design's seam faults remain distinct from Core exceptions.
- The sampled type-5 through type-11 tables match the pinned source's branch order and reject classes. The covenant's separate output and spend call sites, and the unconditional BLS checks for types 7 and 9, are assigned to the intended owners.
- The v24-only profile, the `pprev` MN list, the expired asset-unlock retry's tip-derived BLS flag, and EV1-4's BS12 serial-verifier gate agree with the referenced Core paths. The findings above identify the remaining binding and gate details.

CHANGES-REQUIRED
