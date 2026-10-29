# CH3 design review, Sol round 2

Reviewed only `git diff b5ede63b5 c5a271265` in `docs/node-program/designs/CH3-chainstate.md`, the Sol and Opus round-1 reviews, and the manager's rev2 rulings. I did not edit the branch.

## New Major finding

### M1. G1 misses a pre-DIP3 coinbase with raw CbTx type bytes

**Location:** §2.17 G1 and D31–D32; §2.2 step 15; §3.1 H4.

G1 refuses a CbTx-typed coinbase only when `nVersion >= 3 && nType == TRANSACTION_COINBASE`. D32 ports `CheckSpecialTxInner`'s `bad-tx-type-dip3-inactive` gate, which applies only when `HasExtraPayloadField()` is true (`nVersion >= 3 && nType != 0`). It then skips a coinbase whose raw `nType` is `TRANSACTION_COINBASE`, matching Core's loop skip.

A transaction with `nVersion = 2` and raw `nType = 5` is representable: `dash/src/blockdata/transaction/mod.rs:621-635` decodes it as `ClassicalWithNonStandardVersionTypeBytes(5)`, preserving the raw type (and has a round-trip test at `:1145-1164`). Below DIP3, Core's `ContextualCheckTransaction` has no type check (`validation.cpp:356-378`). With script checks on, Core's `ProcessSpecialTxsInBlock` checks *any* coinbase whose raw `nType == TRANSACTION_COINBASE`, without a version condition (`evo/specialtxman.cpp:816-830`). Its empty extra payload fails `GetTxPayload<CCbTx>` and gives `bad-cbtx-payload`. The version-2 coinbase then skips the later per-transaction loop (`:865`), so D32's ported gate does not see it either. As written, G1 does not refuse it and `NoEvo` can accept it. A valid pre-DIP3 block otherwise passing the ordinary checks therefore diverges from Core under `-assumevalid=0`.

**Required change:** Make G1's CbTx coinbase predicate use the raw `nType == TRANSACTION_COINBASE` at every transaction version, or port Core's top-level CbTx check for this case. Keep D32's `HasExtraPayloadField()` predicate for other raw type bytes: Core accepts some pre-special-version transactions with nonzero type bytes. Add a version-2/raw-type-5 coinbase to the G1 boundary test and compare its Core outcome. This is Major because the proposed fail-closed boundary does not cover every block `NoEvo` cannot judge.

## Round-1 closure table

`PARTIAL` is limited to the pre-DIP3 CbTx case above. The other rows have a concrete design answer matching the ruling; their implementation gates remain future work.

| §12.1 finding | Verdict | Reason if partial |
|---|---|---|
| Sol B1 = Opus M1 | **PARTIAL** | G1 misses version-2/raw-type-5 coinbase; M1 above. G2 and the startup/restart paths otherwise follow ruling 1. |
| Own D32 pre-DIP3 gate | **PARTIAL** | The ported inner gate is correct, but the claimed `NoEvo` exactness below DIP3 overlooks Core's top-level CbTx branch. |
| Sol M1, commit after effects | CLOSED | D33 keeps both parent views uncommitted through `finish_attempt`, with boundary fault injection. |
| Sol M2 = Opus m9, MuHash seed | CLOSED | D34 starts per-block parity at genesis and defines verified segment seeds and the 45-minute fallback. |
| Sol M3 = Opus m2, recorded cursor | CLOSED | D35 advances only on `is_conflict`; removal consumes no record. |
| Opus m1, chained IS locks | CLOSED | D35 includes the chained-removal closure and its test. |
| Opus M2, VerifyDB view | CLOSED | D36 adds `ViewTarget` now, assigns the scratch view and `verifychain 4` to CH5. |
| Opus M3, FT ownership | CLOSED | D37 assigns all eight FTs, wiring slices and dependencies. |
| Sol m1 = Opus m4, slice dependencies | CLOSED | D38 orders a8 before a3 and adds the a7/a10 dependencies. |
| Sol m2, F01 V pause | CLOSED | D39 makes V durably flush and verify the index record before parking. |
| Sol scope note, pool exactness | CLOSED | D25 states this is mempool parity, not a validity or UTXO property. |
| Opus m3, H3 and reward era | CLOSED | D40 retires H3 and uses deployment queries. |
| Opus m5, inline queue control | CLOSED | D41 names the CS6 API and its owner. |
| Opus m6, `fChecked` | CLOSED | D42 carries the memo through the seam. |
| Opus m7, probe before script copy | CLOSED | D43 names both CS6 APIs and the soft-erase test. |
| Opus m8, assumevalid-on run | CLOSED | D44 preloads headers, records choices, and asserts the skip count. |
| Opus m10, undo size header | CLOSED | D45 uses the size only as a hint and falls back to stream decode. |
| Opus m11, unlink and handle cache | CLOSED | D46 opens undo files by path for every read. |
| Opus m12, H7 order | CLOSED | D47 requires pre-commit notification or a listener proof. |
| Opus m13, reverse leg and fuzz | CLOSED | D48 names the mainnet-scale reverse leg and three targets. |
| Opus m14, L3 Core regime | CLOSED | D49 fixes flags, sync state and an assertion before comparison. |
| Opus n1, governance wording | CLOSED | D18/D53 declare the decision-stream and log difference. |
| Opus n2, leaf lock | CLOSED | D50 uses `try_lock().expect`. |
| Opus n3, arena | CLOSED | D51 selects `bumpalo`, with a reviewed fallback. |
| Opus n4, bench split | CLOSED | D14/D53 declare the debug timing split. |
| Opus n5, AbortNode ownership | CLOSED | D52 gives the connector one reporting rule and leaves tip/ABC calls to V. |
| Opus n6, testnet CSV note | CLOSED | D53 states testnet's L2 window ends before CSV. |
| Opus n7, pool scope | CLOSED | D25/D53 limit exactness to mempool parity. |

## Declared choices

| Choice | Sol r2 view |
|---|---|
| G1 omits DIP0008, V19, DIP0024 and BRR | Accept those omissions on the cited Core paths. **Reject the separate version-qualified CbTx trigger** (M1). |
| D32 ports `bad-tx-type-dip3-inactive` | Accept that port, but reject the resulting claim of complete pre-DIP3 `NoEvo` coverage until M1 closes. |
| G2 refusal is `AbortNode`; R10 monthly live-mainnet stop | Accept as the ruled fail-closed behavior, with an explicit operational risk. |
| ND1 has no blanket mainnet refusal | Accept; the connector and startup predicates are the relevant boundaries once M1 is fixed. |
| Undo hint/stream fallback | Accept; the fallback preserves Core's behavior for a damaged size header. |
| Commit after effects; undo write before effects | Accept under D33's latch, recovery and fault-injection contract. |
| CH3a a11 closes last | Accept; the stated RP1a→CH3b dependency is acyclic at slice level. |
| `feature_llmq_is_cl_conflicts` gates CH7b | Accept; the real-lock FT needs CH7b, while CH3a has directed and recorded IS-loop tests. |

No other new Blocker or Major issue was found in the rev2 delta.

VERDICT: CHANGES-REQUIRED
