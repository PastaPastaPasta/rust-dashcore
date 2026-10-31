# LQ1 rev1 design review r2 — independent GPT review

Reviewed `designs/LQ1-quorum-members.md` rev1, SHA-256 `96a2940279d485a9bb3b0455c452a1a3c83a249db4fa797e07bb91fc6c3db0e6`, after reading its changes and §10 dispositions. Read the two r1 reviews, the reviewer contract, CHARTER/PROCESS, DEC-29 D4, DEC-48, DEC-50 and the reconciled EV2 member-source contract. Checked source with `git show` at **v23.1.8** (`728f5055836c6d29806412fc7223ac8fe05af991`) and **v24.0.0-rc.3** (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`). This is a design review; there is no LQ1 implementation to certify.

Two majors remain. The repaired scan design passes my focused checks; the repaired rotation-memo key covers successful computations' value inputs. Neither observation establishes the separate claim that existing snapshot rows are immutable under changes in those inputs.

## Findings

### M1 — major: write-if-absent snapshots rely on an invariance that the modifier contract explicitly permits breaking

**Where:** `designs/LQ1-quorum-members.md:595`, `:603`, `:607`, `:611`, `:796`, `:831`.

The runtime sink refuses to overwrite an existing snapshot and debug-asserts equality, on the premise that every rewrite has identical bytes. The members cache is also called unobservable because members supposedly depend only on its keys. But §3.7.3 explicitly allows work-block data availability to change, and §5.3 correctly includes the newly computed modifiers in L4's key for precisely this reason. The availability is absent from the claimed invariants at :595 and :611.

**Concrete failure scenario:** compute and store a post-V20 rotated cycle while its work-block coinbase contains a non-null CL. Later, its block data is pruned. A historical `quorum info`/members computation with a cold or evicted members cache now uses the work-hash modifier. The old MN lists and snapshot rows can remain available in EvoDB. Changing the modifier can change both reconstructed previous quarters and the score positions at which `BuildQuorumSnapshot` writes its active bits. Core then **overwrites** the cycle's existing row. Rust's proposed sink retains the old row, or asserts in a debug build. Subsequent reconstructions therefore read different inputs on the two nodes. No disk fault or corrupted row is required by this scenario.

**Core evidence:** `llmq/utils.cpp` v23:88–111 / v24:87–115 chooses the CL/hash modifier; `evo/cbtx.cpp` v23:230–232 / v24:192–194 returns no CL when the block read fails; `BuildQuorumSnapshot` v23:303–337 / v24:307–341 places bits in the newly scored order. `StoreSnapshotForBlock`, `llmq/snapshot.cpp` v23:283–292 / v24:320–328, unconditionally calls raw DB `Write` and replaces its cache value. It does not assert equality or write only when absent.

**Focused proof:** compiled the actual score, snapshot-builder and snapshot-quarter reconstruction functions extracted from both tags, with synthetic list/ancestry adapters and two supplied derived modifiers. For the same eight-member list and used set, the active bitsets changed from `00110010` to `00101010`; reconstructed quarters changed from `[3], [5]` to `[6], [7]`. Both tags agreed. This proves the downstream dependency, not a complete running-node pruning reproduction; the modifier switch itself follows from the source and the design's availability contract.

**Fix:** preserve Core's overwrite semantics in `RuntimeSnapshotSink` and the corresponding persistence contract. Withdraw or qualify the identical-rewrite and cache-unobservability claims. Core's members cache must retain its actual warm/cold behavior rather than be justified as a function of base hash alone. Add a stateful test that fills with CL data available, changes availability, forces a members-cache miss, and compares the overwritten row and subsequent reconstruction against Core. Exercise the warm-cache case separately. L4's derived-modifier key should remain: it correctly distinguishes these recomputations.

### M2 — major: Apple's largest known error is not the bound needed to prove count 32

**Where:** `designs/LQ1-quorum-members.md:59`, `:707`, `:709`, `:712`.

The binade argument is sound **if** the actual library has an established error bound below one ulp at this input. A published **largest known error** from numerical testing does not establish that bound for every input, architecture, or system-library version. The cited Apple result therefore does not prove that both shipped macOS architectures return exactly `5.0` for `log2(32)`. The related evidence document, `designs/CS2-fp-release-binaries.md:358`, explicitly distinguishes the study's M1 measurement from the absent x86_64 measurement and describes its Apple argument as an assumption.

**Concrete uncovered outcome:** a library result of `nextafter(5.0, -infinity)` truncates to 4 at Core's call site. With `b = 20 << 58`, Core would select index 10 rather than Rust's index 20. I am not claiming that an actual Apple library returns this value. The point is that the cited evidence has not excluded it, while the design claims proof and confines all remaining gaps to regtest/devnet. This unsupported claim covers the mainnet/testnet count too.

**Fix:** keep `u32::ilog2`, but classify the Apple count-32 result as an evidence gap or explicit manager-approved assumption until measured or proven. Obtain runtime probes of both 2 and 32 on x86_64 and arm64 macOS, with system-library versions and the release call/conversion paths recorded; scope any measurement claim to those libraries. Do not silently count this gap as closed by the existing "count-2 only" list. Linux's source bridge and the Windows evidence do not close the Apple gap.

### m1 — minor: memo-key construction must stop reads at the same point as Core

**Where:** `designs/LQ1-quorum-members.md:829`–`:833`.

The key's "until the first absent row" form is correct, but the instruction to "compute the four modifiers ... before the lookup" contradicts it. With early regtest activation and cycle base 24, `c_1` is genesis: its missing row makes Core break before calculating its modifier. Eagerly calculating `m_1` asks for work height −8; `c_2` and `c_3` do not exist. Similarly, an error in an unreached older work-block read must not fail a computation Core would complete.

**Fix:** specify the read sequence explicitly: current inputs first; then at most three prior cycles, resolve ancestor, read row, stop on missing ancestor/row, and only then obtain that row's work list/modifier. Read no inputs beyond the stopping marker. Add a first-cycle and injected-unreached-read-error test. This is a clarification of the repaired key, not a missing successful-value key component.

### m2 — minor: correct the store-error rationale and distinguish expected absence

**Where:** `designs/LQ1-quorum-members.md:357`, `:369`, `:779`.

The agreed EV0 trait and fail-stop mapping are appropriate, but "v24 does this" overstates Core: only `EvoDbInconsistencyError` takes `state.Error`; other DML store exceptions can still reach `failed-procspectxsinblock` at v24 `evo/specialtxman.cpp:989`–`:996`. Reconciled EV2 already corrects this at `designs/EV2-dml.md:532`; LQ1's statement that EV2 currently maps every `MemberError` to a verdict is stale. The enum doc also calls a missing snapshot row corruption, while §3.6 intentionally treats absent rows as a normal break, including young chains and 28e.

**Fix:** describe universal fail-stop handling as the manager's ruling under both profiles, using Core's typed exception as a precedent. Update the EV2 cross-reference. State that `Ok(None)` for an expected absent snapshot remains normal; unreadable rows and missing records required by an established store invariant become `Store` errors. Test both paths.

### m3 — minor: the revised order gate still does not check entry-state provenance

**Where:** `designs/LQ1-quorum-members.md:348`–`:351`, `:877`, `:892`, `:905`–`:906`.

The trait correctly returns complete `Arc<Dmn>` entries with historical state. However, the explicit oracle output and order gate compare proTxHashes. An implementation that retains the correct ID order but re-resolves a rotated member in the current list passes those checks. If that member is a non-signer in the historical certificate, its changed operator key is not checked by `membersSig` either; its changed service can still affect DK1/LQ5.

**Fix:** add a rotated fixture with operator-key/service updates between the prior work list and the current one. Compare returned state per position, using Core's `quorum info` `pubKeyOperator` and address fields, or extend the oracle output to include those fields. Keep the proTxHash order comparison as the order gate.

## Checks that passed / disposition assessment

- **Selection rules:** checked single-SHA256 scoring with the stored `confirmedHashWithProRegTxHash`, bans/null-confirmation/Evo filters, arithmetic score ordering versus memcmp hash ordering, descending collateral tie-break, the two V20 activation tests, list-block choice, rotation quarters in 3C/2C/C/new order, used-set insertion order and independent swallowed `AddMN` exceptions, short/empty new quarters, skip zero sentinel and signed deltas, modes 0–3, total-sized active bitsets, and snapshot emission before later commitment failure. Rev1 describes these correctly at both pins.
- **Scan/cache:** checked window snapping, cached-prefix continuation, round-based index ordering/truncation, overwrite-on-emplace, keep bounds, delayed LRU truncation, and v24 bypass semantics against source. Compiled each tag's actual `ScanQuorums`, indexed-scan routines and `unordered_lru_cache` with mock commitment/chain adapters. An independent model of §3.8 matched **18,006 scan/guard-selection outputs per tag**, including repeated counts, eviction and restart sequences. A young 60_75 fixture produced `none`, then latest index-20 quorum, then the same quorum; TEST_DIP0024 padded `[24,73,49]` to `[24,73,49,24]`. The selection part of this harness models the guard/index lookup; it is not a full quorum/BLS integration test.
- **L4:** assuming one consistent input view and Core's stopping/read sequence, the type, work ancestry, actually consumed modifiers, row contents/presence and process-constant parameters determine the output. Availability/BLS/profile effects on successful modifier values are covered. Re-emitting under the current base hash and not caching errors are correct. M1 concerns the outer runtime cache/sink, and m1 concerns speculative input reads.
- **`log2`:** verified fixed rotated counts `{2,32}`, no count-changing overrides, and archived conversion listings including Windows. Re-ran the existing probe on agentbox's x86_64 glibc 2.43 with default and FMA/AVX2-disabled dispatch: exact powers of two and zero truncation mismatches over 1..65536. Independently checked the archived glibc source grouping and identical table data across 2.29–2.43. The declared unmeasured FMA-target/count-2 gaps remain; M2 adds the missing Apple count-32 qualification.
- **Trait/errors:** one EV0-owned, object-safe member-source trait with concrete historical entries, effects and typed errors fits the reconciled EV2 interface and DEC-50(10). Universal store-error fail-stop is an authorized deviation, subject to m2's precise documentation.
- **Would verification catch an order bug?** Yes, for covered fixtures/history: ordered L1 member outputs and ordered `quorum info` arrays catch permutations, including ones entirely inside the signer set that secure aggregation cannot detect. Rev1 correctly makes `membersSig` only a signer-key-multiset check. Keep the all-quorum/non-vacuity and era gates mandatory; the proposed interim regtest fallback in §8 Q3 must not be reported as completed testnet/pre-V19-mainnet coverage. m3 addresses a different, state-provenance blind spot.

Scratch evidence and executable harnesses are in `/work/scratch/lq1-r2-gpt/`: `make_scan_harness.py`, `check_scan.py`, `scan-results.txt`, `make_snapshot_harness.py`, `check_snapshot.py`, `snapshot-results.txt`, and the extracted tag sources. Also ran **100,000** model encoder/decoder trace round-trips covering the zero sentinel, wrap and negative deltas. The snapshot harness uses supplied derived modifiers and synthetic adapters; neither harness replaces the planned full oracle/functional tests. No live oracle RPCs or datadirs were touched, and no source branch, commit, push or GitHub comment was changed. Tests used `TMPDIR=/dev/shm/lq1-gpt-tmp` (0700), removed after completion.

## Verdict

**APPROVE-WITH-CHANGES.** Resolve M1's runtime snapshot/cache contract and M2's evidence classification before approving rev1. Fold m1–m3 into the design and implementation acceptance criteria.
