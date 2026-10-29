# LQ1 rev2 design review r3 — GPT

**Verdict: APPROVE-WITH-CHANGES.** Two majors and three minors. The snapshot overwrite rule and the abstract members-cache algorithm are now correct, but the public precompute and scan APIs do not yet carry the runtime operations needed to make their write points match Core. Resolve findings 1–2 before approving the design; fold findings 3–5 into the contracts and acceptance criteria.

Reviewed `designs/LQ1-quorum-members.md` **rev2 at d44631bb**, SHA-256 `99ef8948091751dbf88ab8b185612ac1ca3b4f7b81f7894ea1f5b20ac22c2080`. Read the reviewer contract, all three prior reviews, DEC-29 D4, DEC-41, DEC-50(10), DEC-54, and the relevant EV2 interfaces and store mapping. Verified Core with `git show` at **v23.1.8** (`728f5055836c6d29806412fc7223ac8fe05af991`) and **v24.0.0-rc.3** (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`). Core citations below are under `src/`. The integration branch advanced during this review; the LQ1 file remained byte-identical to the reviewed commit. Dependent documents at d44631bb are archived in scratch.

This reviews a design, not an implementation. “Closed” below means the design now supplies the required contract and gate, not that its future implementation or history replay has passed.

## Numbered findings

### 1. Major — precompute cannot use or reset the shared members cache through the proposed API

**Location:** `designs/LQ1-quorum-members.md:301–305`, `:395–398`, `:416`, `:626`, `:637–640`.

The cache algorithm in §3.7.4 is correct, but §2.5 still exposes `precompute_quorum_members(ctx, src, block, memo, rows)` as a pure function and instructs LQ2 to call it in both `process_block` and `undo_block`. Its inputs provide lists, coinbase facts, a sorted-list memo, and snapshot rows. None exposes the shared members cache or a cached member-source callback, and there is no `reset_cache` argument. The EV0 member-source method also has no reset operation. The only specified cached wrapper is around `QuorumMemberSource::members`.

Consequently, implementing the public precompute function with the uncached `get_all_quorum_members` contradicts §3.7.2's claim that every production computation goes through Core's cache. Merely adding a reset boolean to that pure function would not give it access to the runtime cache either. A runtime entry point and its routing must be specified.

**Core, both pins:** `PreComputeQuorumMembers` calls the **cached** `GetAllQuorumMembers(..., reset_cache)` for each eligible type in configured order: v23 `llmq/blockprocessor.cpp:30–38`, process `:176`, undo `:387`; v24 `:34–42`, process `:223`, undo `:523`. `GetAllQuorumMembers` returns on a base-cache hit, clears that type's base cache on reset, and then clears/checks the indexed cache only on the valid rotated path: v23 `llmq/utils.cpp:576–643`; v24 `:641–714`. Successful computations populate both caches before returning: v23 `:639–658`; v24 `:710–729`.

**Concrete failure scenario:** use the design's own availability-changing fixture. Warm the process-wide cache for a rotated cycle with the CL modifier, change the work-block availability to select the hash modifier, then run precompute with normal connect semantics. Core's hit returns the warmed members and does not overwrite the row. The proposed pure route recomputes and overwrites it. On undo, calling the ordinary cached `members` method instead gives the opposite problem: no reset, so Core's required recomputation is skipped. Calling the pure function on undo computes a row but does not clear/repopulate the runtime caches; a subsequent member call can return the old cached members.

This is a concrete adapter/API counterexample using the source's declared mutable input, not a claim that an implementation already exists or that an ordinary tip template's eight-block-old work block can be pruned in place. The new cold/warm oracle fixture must exercise the **production precompute facade**, not only the pure function and `oracle_llmq_members` separately.

**Evidence:** the extracted actual cached functions and an independent cache model agree at both pins. In the focused trace, after warming cycle 240 at input revision 1 and changing the synthetic computation to revision 2, `PRE 240 0` leaves row 1; `PRE 240 1` stores row 2 and subsequent member calls return revision 2. The uncached route `PURE 240` stores row 2 while the shared cache still returns revision 1. See `members-warm-and-undo-*`, `members-pure-api-bypass-*` and `members-check-summary.txt`. The synthetic revision isolates routing; the separate snapshot harness proves that a real derived-modifier change can change actual row bytes and reconstructed members at both pins.

**Suggested fix:** define CH7b's runtime precompute entry point, taking the attempt sink and the connect/undo reset mode, and have LQ2 call that entry point. Let the pure part enumerate eligible types/bases, or inject a cached callback into it. Every eligible call must reach the same process-wide facade as RPC/DKG/commitment callers, with `false` for connect/template and `true` for undo. Specify that cache mutations survive a later failed/just-check attempt, that undo resets only the eligible types at cycle-base heights, and that the Core gates and indexed-cache reset order are preserved. Keep uncached precompute only as an explicitly named test/reference primitive.

### 2. Major — a commitment-only scan omits the quorum builds that warm the members cache and rewrite rows

**Location:** `designs/LQ1-quorum-members.md:247–251`, `:311–321`, `:333–342`, `:657–660`, `:674`, `:952–964`.

§3.8 models the scan's base/commitment values, continuation and LRU correctly. However, `scan_quorums_from` can only read `MinedCommitmentIndex`; `ScannedQuorum` contains a commitment and block identifiers, and `ScanOutcome` contains result/cache-write vectors. The specified QM1 facade performs lookup → pure scan → emplace. There is no operation for Core's ordered `GetQuorum` calls, no member/effect sink, and no retained quorum-object identity in its cache contract. Reading a mined commitment is not equivalent to building a `CQuorum` now that the builds can change the observable members-cache state and snapshot rows.

**Core, both pins:** a scan miss requests at least `keepOldConnections`, then calls `GetQuorum` for **every** newly scanned base before truncating the public result: v23 `llmq/quorumsman.cpp:263–306`; v24 `:237–285` (the build loop is `:248–270`). `GetQuorum` checks mined availability, consults the quorum-object cache, and builds only on an object-cache miss: v23 `:391–414`; v24 `:518–536` / chain overload `:539–555`. A build calls the shared `GetAllQuorumMembers`: v23 `:66–82`; v24 `:58–74`. The scan cache stores `CQuorum` pointers, whose member entries remain those captured by that build; a sufficiently long scan-cache hit returns them without a new `GetQuorum` or member-cache access (v23 `:245–254`; v24 `:208–217`).

**Concrete failure scenario:** on a cold node with two available rotated commitment rounds, run a count-1 scan. Core builds across the keep-sized range, including the previous cycle, even though only one base is returned. If the previous cycle's work data was pruned after its existing row was written, that build can overwrite the previous cycle's row with different bytes. A commitment-only scan performs no overwrite; a facade that builds only the returned prefix misses the older cycle's overwrite. The base list and selected hash can nevertheless be identical, so the current scan/selection fuzz comparisons pass. The different cache warmth also changes later recomputations and RPC member answers. Conversely, rebuilding members for a scan-cache hit would add accesses/writes Core does not perform.

The source trace uses the smaller TEST_DIP0024 shape: a count-1 miss with four commitments calls `GetQuorum` for bases **48, 49, 24, 25**, in that order, and returns only **48**. Repeating the same scan returns 48 with **zero** `GetQuorum` calls. Both pins do this. See `scan-trace-input.txt` and `scan-trace-results.txt`. This establishes the materialization stream; the actual changed-row dependency is demonstrated separately by the snapshot harness.

**Suggested fix:** extend the scan/runtime boundary so QM1 executes Core's ordered quorum materialization while scanning, including entries outside the returned prefix, the exact `populate_cache` decisions and the v24 chain overload. It must preserve object-cache hits and the quorum objects retained by the scan cache. A callback/separate scan plan with an incremental materialization step is suitable; a final truncated result alone is not. If a later build fails, keep earlier member-cache/snapshot effects even when the scan returns empty; do not describe “no scan-cache write” as “no effects.” Add cross-cache differential sequences comparing **GetQuorum/member-cache access traces and written rows**, as well as returned bases, for cold scans, hits, continuation, discarded tails and a later failure. The oracle scan context must use the real shared member/snapshot managers, not a commitment-only substitute.

### 3. Minor — the L4 stopping rule is fixed, but its claimed exact read order matches neither pin

**Location:** `designs/LQ1-quorum-members.md:891–903`, `:933`.

The repaired rule stops before a missing prior row's list/modifier and reads nothing from older cycles. That closes the dangerous speculative-old-input behavior. The first numbered step nevertheless imposes `list(work_0) → m_0 → prior rows`, while explicitly citing source that puts `m_0` elsewhere.

**Core, both pins:** v23 reads the current list, reconstructs the reached previous cycles, then computes `m_0` inside the new-quarter builder: `llmq/utils.cpp:468–494`, `:354`. v24 computes `m_0` in `GetAllQuorumMembers` **before** entering the rotation function, which then reads the current list: `:701–707`, `:484`. Each prior-cycle helper reads its list then its modifier at v23 `:197–201` / v24 `:201–205`, after the row read at v23 `:477–491` / v24 `:491–509`.

**Concrete failure scenario:** with row H−C present and row H−2C absent, the extracted traces are:

- v23: `list_0 → row_1 → list_1 → m_1 → row_2(absent) → m_0`;
- v24: `m_0 → list_0 → row_1 → list_1 → m_1 → row_2(absent)`;
- rev2: `list_0 → m_0 → row_1 → list_1 → m_1 → row_2(absent)`.

An injected `m_0` read error stops rev2 before the reads v23 would already have made; an injected `list_0` error skips the modifier read v24 would already have made. The successful-input key remains complete; I have **not** demonstrated a different successful member result here. This is an exact-sequence/error-trace documentation gap, not another missing key component. Evidence: `make_read_trace.py`, `read-trace-results.txt`.

**Suggested fix:** state the actual per-pin ordering and use it in the traced reader, while keeping the same eventual key layout and ancestor/row stopping markers. Alternatively, explicitly qualify the order claim and justify the permitted reordering under the declared fail-stop policy; do not call the current sequence an exact port. Test the traces with a first missing row, one reached row followed by absence, and an error in each reached input. The unavailable older reads must remain untouched.

### 4. Minor — the row-digest oracle has no canonical key/framing contract

**Location:** `designs/LQ1-quorum-members.md:856–860`, `:945–955`, `:1016–1018`.

`SHA-256 over all (key, row) pairs in key order` does not say which keys or serialized framing to use. Rust's CF key is `(type, cycle-base hash)`; Core's key uses a hash of that pair. Their native key order and byte streams differ even for exactly equal logical rows. The gate is not independently implementable as one unambiguous comparison yet.

**Core, both pins:** `StoreSnapshotForBlock` derives `snapshotHash = SerializeHash(pair(type, baseHash))` and writes `(DB_QUORUM_SNAPSHOT, snapshotHash)` at v23 `llmq/snapshot.cpp:283–291` / v24 `:320–328`; the namespace is `llmq_S` at v23 `:18` / v24 `:24`. Row encoding is `llmq/snapshot.h:41–89` / `:42–90`. This is intentionally different from the Rust storage key in §4.2, under DEC-11.

**Concrete failure scenario:** one shim implementer hashes the native Core serialized DB keys and another implementer hashes Rust's native CF keys, each in its own byte order. Every digest comparison can fail despite equal rows. If a harness compares only recorded input pairs instead of enumerating the store namespace, it can instead omit rows created under template/failed-attempt hashes and miss the behavior this gate was added to catch. `digest-ambiguity-results.txt` demonstrates different digests for the same two logical rows under the two native-key readings.

**Suggested fix:** publish one exact preimage and a known-answer vector. For example: normalize Rust keys to Core's serialized `llmq_S` key, sort those bytes lexicographically as unsigned bytes, and hash `CompactSize(row_count) || Σ(CompactSize(key_len) || key || CompactSize(row_len) || canonical_snapshot_bytes)`. Include every row in that namespace, including seeded, overwritten, failed-attempt and template rows; exclude other EvoDB namespaces. State empty-store behavior, the coherent read boundary, and that computing the digest does not warm the members cache or generate rows. Also define the written-row output's key encoding, ordered-event framing and per-call capture boundary. `reset_cache` itself is sufficiently constrained as a flag forwarded to Core: document that it is a per-type member-cache reset with Core's gates, not a snapshot deletion or scan-manager reset.

### 5. Minor — EV2 still describes expected snapshot absence as a fail-stop store error

**Location:** `designs/LQ1-quorum-members.md:377–394`; `designs/EV2-dml.md` at d44631bb `:505–506`, `:544`, `:763` (the duplicated member-source comment remains in the subsequently advanced file too).

LQ1 now correctly distinguishes `Ok(None)` and unavailable block data from read/corruption errors. EV2's copied member-source comment still says “A missing or unreadable list, snapshot row, mined commitment or block” is `Store`; its mapping paragraph likewise says a missing “row or block” therefore returns `Store`. The **mapping once a Store error exists** agrees across both designs. The classification of normal absence still needs to agree, especially because EV0 is to adopt one shared contract.

**Core, both pins:** `GetSnapshotForBlock` returns `nullopt` for no row, v23 `llmq/snapshot.cpp:264–280` / v24 `:301–317`; normal rotation breaks at v23 `llmq/utils.cpp:479–485` / v24 `:496–503`. Core's exception catch is v23 `evo/specialtxman.cpp:700–704`; v24's typed-error and general catches are `:989–996`. Universal fail-stop remains the authorized ruling, not literal v24 DML behavior.

**Concrete failure scenario:** an EV0/EV2 implementer follows the duplicated EV2 comment and converts the expected missing previous-cycle row on a young rotated chain into `MemberError::Store`. The node aborts instead of computing the remaining/new quarter that Core and LQ1 describe. Treating a pruned work block as a missing required store record similarly prevents the specified hash-form fallback.

**Suggested fix:** have the reconciler update EV2's duplicated comments, absence classification and reference from LQ1 rev1 to **rev2 §2.6**. Specify that only a read failure or a missing record required by an established invariant is `Store`; normal missing snapshot rows and `BlockDataUnavailable` are values. Keep the shared `Store → DmlBuildError::Store → node error` mapping and the normal-absence/store-error test pair.

## Closure of every r2 finding

| Prior finding | Status | Evidence/disposition |
|---|---|---|
| GPT M1: write-if-absent / cache unobservability | **Partially closed** | §§2.3, 3.7.1–4 and 4.2 restore unconditional overwrites, the overlay, runtime writes and correct abstract cache semantics; D3 is withdrawn. The Core-faithful production call stream remains incomplete at the precompute and scan boundaries (findings 1–2). |
| GPT M2: Apple known error is not a proof | **Closed** | §0 item 6 and §§3.10.3, 3.10.5 explicitly leave Apple count 32 open, scope future measurements by OS/architecture, and make no macOS parity claim. The external Mac measurement is still outstanding, as the manager ruled; closing this finding does not close that evidence gap. |
| GPT m1: eager unreached old-input reads | **Closed for the reported stopping bug** | §5.3 steps 2–3 stop at ancestor/row absence before that cycle's list/modifier; §§6.1 and 7 G2 require the first-cycle/unreached-error vectors. The trace harness confirms the same reached previous inputs. Finding 3 addresses the remaining inaccurate global-order claim. |
| GPT m2: store rationale, stale mapping, expected absence | **Partially closed** | LQ1 §2.6 and D4 now give the ruling/typed-Core precedent accurately, correct the error mapping, and distinguish expected absence. EV2's mapping agrees, but its duplicated absence documentation remains stale (finding 5). |
| GPT m3: entry-state provenance untested | **Closed** | §§6.1, 6.2 and 6.3.2 add changed-key/service fixtures, member-state oracle output and per-position operator-key/address comparisons; LQ1b-E acceptance explicitly includes them. Core RPC reads those fields from the stored member entry at v23 `rpc/quorums.cpp:209–214` / v24 `:217–224`. |

The r2 coverage note is also closed at the design level: §6.3.2 reports network/era coverage separately and never counts regtest as completed testnet/mainnet coverage.

## Closure of every r1 Opus major and minor

A = `LQ1-design-r1-opus-core.md`; B = `LQ1-design-r1-opus-arch.md`.

| Prior finding | Status | Rev2 evidence |
|---|---|---|
| A-M1: scan continuation reaches P2P | **Closed for the reported continuation issue** | §§2.5, 3.8.1/3.8.3 and 3.9 model the prefix, continuation, padding, overwriting LRU and selection reach. The extracted scan body matches 18,006 value/selection checks per pin. Finding 2 is the newly observable materialization/effect boundary, not a recurrence of the padding error. |
| A-M2: shared coinbase-CL predicate mis-scoped | **Closed** | §3.3.1 supplies one EV4/BS6a predicate under profile/BLS decoding, all five consumers, EVO-06/EVO-18 links and the §6.3.5 invariant; §7 assigns the fallback owner if EV4 has not landed. The final O17 row id is still a manager integration item (§8 Q1). |
| A-m1: 30b rationale/contradiction | **Closed** | §3.9 and the 30b/30c register give the scanned-count guard and first matching index; §6.1 requires the rejecting-present-index vector. Reproduced cold/warm selection and duplicate padding. |
| A-m2: log2 listings/source bridge | **Closed** | §§3.10.2–3 archive the Windows call sites, add the glibc bridge/2.39 measurements and name unmeasured count-2 variants; rev2 now also states the Apple count-32 gap. No unmeasured cell is promoted to proven parity. |
| A-m3: pre-V19 reconstruction != original new quarter | **Closed** | §3.6.3 28d and LQ1b-D compare against quarters embedded in later quorums, not the earlier newly built quarter. Source builder/decoder semantics unchanged from the r2 verified path. |
| A-m4: signed selection-start input | **Closed** | §2.5 takes `i32`, recognizes −1, computes subtraction in `i64`, and bounds the resulting height; §3.9 preserves the caller semantics. |
| A-m5: decoder bound | **Closed** | §§3.6.2 and 6.5 use `N·size/4 + skip_list.len()`; the decoder advances by a member push or skip consumption. Re-ran 100,000 encoder/decoder traces. |
| B-M1: competing member-source traits/error mapping | **Closed for shape and mapping** | One EV0-owned object-safe concrete trait with an effect sink and `MemberError` at §2.6; EV2 §2.11 agrees on its signature and mapping. Finding 5 is the remaining copied absence classification. |
| B-M2: incomplete L4 key | **Closed for successful-value inputs** | §5.3 keys on type/work ancestry, consumed derived modifiers, reached row digests/presence and the stop marker; no errors are cached, and a hit re-stores under the current cycle hash. L4 sits below the Core-faithful member cache. Finding 3 qualifies only the order claim. |
| B-M3: Core perf gate not set up | **Closed at design level** | §5.4 assigns the shared-loader Core driver to LQ1a-A, first members comparison to LQ1a-B, labels eras/block-read paths, allows recorded manual comparisons and makes O2a-LQ1/PF0 prerequisites. This does not certify performance before the driver exists. |
| B-M4: certificate/arbitration too strong | **Closed** | §6.3.2 makes `membersSig` a signer-key-multiset check, makes ordered/stateful `quorum info` comparisons the gate, and §6.4 requires traced gap triggers and non-vacuity. |
| B-M5: live oracle copy/writing reads | **Closed** | §6.3.6 uses the O7 replica for writing/heavy calls, rejects live-datadir reflink copies, lists pure reads, throttles them and reclassifies the r0 probe. No oracle calls were made in this review. |
| B-M6: unreachable dash-spv helper | **Closed** | §6.4 inverts the dependency: LQ1b-F writes data fixtures, an owned in-crate dash-spv test consumes them via `test_support`. |
| B-m1: full scratch-list cost / work-block retention | **Closed at design level** | §2.2.4 specifies lean `UsedSet`, exact `try_add` failure order and an `MnList::scratch()` twin; §8 Q4 requests `WorkBlock` from EV2. That cache-hint addition remains an explicit implementer/reconciler follow-up. |
| B-m2: historical entry state/operator access | **Closed** | §2.6 binds concrete consumers to returned `Arc<Dmn>` entries and prohibits current-list re-resolution; §2.2.3 separates selection inputs from downstream keys/services. Rev2 adds the actual provenance gate. |
| B-m3: iteration-order test | **Closed** | §6.5 compares memcmp, EV2 `core_order()` and reversed iteration; §7 E includes it. DEC-54's selected online-history target does not introduce a new LQ1 iteration dependence. |
| B-m4: dependencies/BS8/cycle | **Closed** | §2.5 uses an associated commitment type; §7 lists CS0/CS4/BS6a/BS11a-t/O2a-LQ1/PF0 and EV4, puts BS8 only on LQ1b, and depends on EV2a rather than EV2b. |
| B-m5: L5 decoded values/miss/prune | **Closed** | §5.3 stores raw coinbase facts, decodes with the caller, falls back to a block read on a miss, checks current HAVE_DATA and limits storage to work heights. D2 explicitly distinguishes disk faults from ordinary unavailable data. |
| B-m6: nonexistent single-SHA256 kernel | **Closed** | §5.3 L2 and §8 Q5 assign SH1b, its scalar twin and architecture tests; no D64 speedup is claimed as evidence for this new kernel. |
| B-m7: lever homes / missing L6 method | **Closed** | §2.5 adds `newest_by_index` and the sorted-list memo hook; §5.3 puts L1/L4/L5 in the runtime. The indexed-round value model passes at both pins. Finding 2 separately covers quorum materialization. |
| B-m8: runtime sinks / overlay / duplicate rows | **Closed by the manager's superseding semantics** | §3.7.2 defines synchronous runtime overwrites and a last-write-wins attempt overlay with ordered unconditional effect application. The old assertion/drop/dedup recommendation is appropriately superseded, not reintroduced. The overlay model passes; findings 1–2 address when it is called. |
| B-m9: enabled-type ownership/overrides/oracle | **Closed** | §§2.5, 3.2, 6.2 and 7 A supply ownership, both override options and `oracle_llmq_type_enabled`. |
| B-m10: non-vacuity/races/30c coverage | **Closed** | §§6.3.2–3 and 6.4 require counts, tip bracketing and historical 30c coverage through constructed/L1 vectors; network/era reporting is explicit. |
| B-m11: missing eras/regtest captures | **Closed** | §6.4 requires per-era/network replica captures, includes `core_v24_regtest_qrinfo.json` and FT-generated captures; these are requirements, not claimed completed coverage. |
| B-m12: G phasing/LQ2 blocked by levers | **Closed** | §7 splits G1/G2 and lets LQ2 start after E; §9 carries that dependency forward. |
| B-m13: stock list-input check | **Closed** | §6.3.1 uses stock CbTx `merkleRootMNList` until O8, names its omitted `try_add` inputs and covers them with L1 plus the all-quorum gate. |

The r1 nits are also addressed: A-n1 at §3.7.1 (default regtest/dual chainstate), A-n2 at §6.2 (process/global/cache constraints), A-n3 at §3.8.3 (LRU mechanics); B-n1/B-n3 at §2.2.4 (EV2 names/SML null collateral), B-n2 at §5.3 L1 (sorted-list memo), and B-n4 at §2.3 (`LlmqCtx::new`). Finding 4 is a new precision requirement for the newly added digest, rather than a missing global-Params warning.

## Answers to the rev2 attack questions

- **Members cache itself:** per configured type, both caches use `keepOldConnections`, not `keepOldKeys`, including the per-cycle-index entries. Truncation happens only above 2× capacity and keeps the most recently accessed capacity entries. `get` and overwrite update recency. It is process-global, is cold after restart, and is not rolled back when a block later fails. Undo reaches resets only for enabled rotated types on cycle-base heights; an ordinary undo does not clear everything. The gate comes before base reset, and the rotated index guard comes before indexed reset. With those guards taken from §3.6, §3.7.4's abstract algorithm matches both pins. What does not yet match is the full production access stream (findings 1–2).
- **Warming:** actual `GetAllQuorumMembers` calls share the statics. Direct commitment checks, PoSe, DKG session init/contribution access, topology/debug, and precompute can warm them. RPC/selection/scans warm them through **quorum-object-cache misses**, not on every high-level RPC or scan invocation. A scan-cache hit can retain a quorum object without even touching that object cache. Templates warm through `TestBlockValidity` precompute and can also touch historical members through the miner's DML build. Relevant direct anchors: commitment v23 `:49,173` / v24 `:53,177`; specialtxman v23 `:456` / v24 `:723`; dkgsession v23 `:88` / v24 `:92`; dkgsessionmgr v23 `:389,438` / v24 `:147,196`; miner's DML build v23 `node/miner.cpp:291` / v24 `:287`. These distinctions must survive the runtime adapters.
- **Snapshot overwrite/failed attempt:** the specified last-write-wins overlay plus ordered post-attempt application reproduces the raw write/get algebra, including a later failure. No equality assertion or write-if-absent rule should return. Applying the log regardless of failure is necessary but insufficient unless cache state/accesses also match. The design must not discard the cache changes from a rejected attempt. This review does not certify WAL crash recovery or fail-stop application errors without CH7b.
- **Memo:** successful-value inputs and prior-cycle stopping set are complete. Exact global read order is not (finding 3). The first missing row stops before its own list/modifier; the missing-ancestor break agrees on the declared reachable domain, rather than emulating v23's unreachable null dereference.
- **Store errors/EV2:** universal `Store` fail-stop is an authorized deviation under both profiles. The error mapping agrees with EV2; expected absence needs the copied EV2 text corrected (finding 5). D2's local disk-fault deviation remains separate from normal pruning.
- **Oracle seams:** forwarding `reset_cache` gives Core's real per-type behavior; it is not an entire-context clean restart. Snapshot seeding through `StoreSnapshotForBlock` correctly maintains the manager's LRU. Digest/event normalization still needs specification (finding 4), and scans must use the real materialization path (finding 2). Add production-facade precompute coverage so the warm/undo tests cannot pass while missing finding 1.
- **Apple:** no Mac evidence was obtained or claimed. The manager's open count-32 classification is correctly implemented. The integer replacement and the profile policy remain as approved.

## Evidence and validation

All evidence is under `/work/scratch/lq1-r3-gpt/`; see its `README.md` for reproduction and limitations.

1. **Members/cache/raw-row harness:** compiled each tag's actual `GetAllQuorumMembers`, `PreComputeQuorumMembers`, `GetSnapshotForBlock`, `StoreSnapshotForBlock`, `unordered_lru_cache`, and v24 `PerLlmqTypeCache`, with synthetic computation/list/DB/chain adapters. An independent model matched **50,000 randomized outputs per pin** plus **26 focused outputs per pin**, including both cache levels, per-type eviction, guard/reset order, computation errors, overwrite behavior and failed-attempt cache retention.
2. **Scan:** rebuilt the r2 harness from this round's pinned source extracts; **18,006 scan/selection outputs per pin** matched. The additional extracted scan trace confirmed full-range materialization and no materialization on a satisfied cache hit. The old harness alone checks scan values, not cross-cache effects.
3. **Snapshot:** rebuilt the actual scoring/builder/quarter-decoder harness at both pins. Changing the supplied derived modifier changed active bits from `00110010` to `00101010` and reconstructed quarters from `[3], [5]` to `[6], [7]`. Re-ran **100,000** encoder/decoder traces. This proves downstream sensitivity, not a full running-node pruning reproduction.
4. **Read sequence:** compiled each tag's actual quarter-rotation function with traced source adapters and read-preserving synthetic quarter helpers. Recorded the distinct orders and identical previous-row stopping set for first-cycle and one-prior-row cases.
5. **Attempt overlay:** an independent overlay/effect model matched **24,176 outputs per pin** against the actual snapshot-manager raw-write/get routines over **2,000 attempts**, **1,336 simulated later failures**, **7,825 writes** and **14,351 reads**. This checks adapter algebra, not full `ConnectBlock` or crash durability.
6. **Digest:** a two-row example records the unequal native-Core/native-Rust digests despite equal logical rows.

No live oracle calls, dashd nodes, source/design edits, commits, pushes or GitHub posts were made. Only this requested review file and designated scratch evidence were written. Tests used `TMPDIR=/dev/shm/lq1-r3-gpt-tmp` (0700), removed after completion.

**Final verdict: APPROVE-WITH-CHANGES.** Complete the runtime precompute and scan materialization contracts before design approval; the manager's overwrite and Apple evidence-gap rulings stand.
