# LQ1 design review r1 (Core-equivalence reviewer, Opus)

| | |
|---|---|
| Design | `designs/LQ1-quorum-members.md` (r0, 2026-10-08) |
| Reviewer role | design reviewer 1 of 2, Core equivalence (DEC-42: GPT unavailable) |
| Core verified at | v23.1.8 (728f5055) and v24.0.0-rc.3 (0364ed62), from `git show <tag>:src/...` in `/work/home/agent/workspace/dash`. Extracts are in `/work/scratch/lq1-rev-a/{v23,v24}` |
| Models run | `/work/scratch/lq1-rev-a/sim/scan_cont.py` and `scan_guard.py`: line-by-line Python models of `ScanQuorums` (cache and continuation), `GetMinedCommitmentsIndexedUntilBlock` and the rotated branch of `SelectQuorumForSigning` (§M1) |
| Verdict | **APPROVE-WITH-CHANGES** (0 blockers, 2 majors, 5 minors, 3 nits) |

## Summary

Selection, ordering, modifiers, rotation, the skip-list codec and the snapshot emission boundary match Core at both pins, as written. The author's corrections to R4 and A1 are right. These include the first non-zero skip entry becoming the base, negative skip entries being legal, the two no-snapshot paths, emission only after the CbTx, special-tx and credit-pool checks, and the rc.3 predicting mode. The `log2` replacement is equivalent on every configurable network for the Linux and Windows release builds. macOS has one cell (count 2) that is argued but not proven, as the author says.

There are two majors:
- **M1.** The `ScanQuorums` continuation divergence is not RPC-only. P2P selection reaches it on young rotated chains.
- **M2.** The proposed coinbase-CL profile row is scoped to the modifier. Core uses a single predicate with five consumers, and one of them is a consensus rule.

## What I verified (no finding)

**1. Ordering, tie-breaks and modifiers**
- `CalculateQuorum` (v23 utils.cpp:157-182 / v24 :161-186): `std::sort(rbegin, rend, cmp)` sorts by score descending, then by collateral descending. Scores compare as `arith_uint256`; `COutPoint` compares by memcmp and then by `n`. Equal (score, collateral) pairs cannot occur in any Core caller, because every input is either a list or an `AddMN` scratch list.
- Both filters match. The list overload uses `onlyValid`, which keeps non-banned entries only.
- `GetHashModifier` (v23 :88-111 / v24 :99-115) and `GetHashModifierFromWorkBlock` match. The `ActiveAfter(work, V20)` / `ActiveAt(work, V20)` split behind the `base = V20+7` row is right.
- Every row of the quirk-29 table holds. Mainnet V20 = 1,987,776 = 576·3451, so mainnet has exactly one mixed base. Testnet has none (905,100 mod 24 = 12, mod 288 = 204).
- The Evo-only rule applies on the non-rotated path only, gated by `ActiveAfter(base, V19)`.
- `skipRemovedMNs` is equivalent at both pins: V19 is buried.

**2. Skip lists**
- Encoder (v24 :416-455): `firstSkippedIndex == 0` is tested on every skip, so a skip at index 0 keeps the base at 0. `idx - firstSkippedIndex` wraps as `size_t` and is then narrowed into `vector<int>`.
- Decoder (v24 :256-265): mirrors the encoder exactly. A negative first entry is reachable only from imported rows.
- Modes 2 and 3 and any unknown mode give empty quarters. Mode 0 wraps.
- The two no-snapshot paths are confirmed: v23 :356 / v24 :362 (`enabled < quarterSize`), and v23 :439-442 / v24 :446-451 (a full pass with no progress).
- An empty combined list with `enabled >= quarterSize` (every masternode unconfirmed) *does* store a mode-0 snapshot. The design's two-path claim covers this correctly.

**3. Emission boundary**

`StoreSnapshotForBlock` (v23 snapshot.cpp:283-292 / v24 :320-330) is a raw `GetRawDB().Write` plus the 32-entry LRU, and only `BuildNewQuorumQuarterMembers` reaches it.

I traced every caller of `GetAllQuorumMembers` at both pins:
- blockprocessor `PreComputeQuorumMembers` (:176/:387 and :223/:523);
- `commitment.cpp` (Verify, and v24's `VerifySignatureAsync` :53);
- the specialtxman PoSe call;
- `BuildQuorumFromCommitment`;
- `dkgsessionmgr` / `dkgsession`;
- `llmq/debug`;
- `GetQuorumConnections` / `GetQuorumRelayMembers`;
- the v23 `EnsureQuorumConnections` / `AddQuorumProbeConnections`.

Findings from that trace:
- Only PreCompute can write a row the first time for a new hash. Every other caller's base is checked to be on the active chain or to be an ancestor of `pindexPrev`. `qfcommit` checks the active chain at blockprocessor.cpp v24 :152, and `CheckLLMQCommitment` checks ancestry at v24 :285.
- The order inside `ProcessSpecialTxsInBlock` (v24 :797-907, v23 identical in order) and the ConnectBlock preamble (CheckBlock, `bad-chainlock`, `VerifyBestBlock`, BIP30, `bad-sb-start`, then ProcessSpecialTxsInBlock at v24 :2663) are as the design says.
- `TestBlockValidity` writes under the template hash, and failed blocks keep their rows.

I found no write path that the design misses at either pin. Two observations go to nits: n1 (dual chainstate) and the default-regtest pre-DIP3 cycles.

**4. `log2`**
- The only consensus-relevant sites are v23 quorumsman.cpp:717 and v24 :739 and :811. The `cuckoocache.h` `log2` is the signature cache's sizing.
- Every rotated type has a fixed count: TEST_DIP0024 = 2, DEVNET_DIP0024 = 2, 60_75 = 32.
- `UpdateLLMQTestParameters` and `UpdateLLMQDevnetParameters` (v24 chainparams.cpp:770-777, :1019-1026) write only `size`, `minSize`, `threshold` and `dkgBadVotesThreshold`, and only for non-rotated types.
- The role overrides (`-llmqinstantsenddip0024`, `-llmqtestinstantsenddip0024`, `-llmqplatform`, `-llmqmnhf`) re-point roles only. `-llmqchainlocks` throws on a rotated type (v24 :1270).
- So the domain is exactly {2, 32} on every network that can be configured. `u32::ilog2` with the BS5 range check is equivalent wherever libm returns exactly 1.0 and 5.0. I spot-checked the listings: for example, `24.0.0-rc.3-x86_64-0x32d4e0.s:268-271` shows `call log2@plt` followed by `cvttsd2si`.

**5. Pin delta**

The only member-selection difference is in `GetNonNullCoinbaseChainlock`: v24 cbtx.cpp:200-211 (commit a0bcedba58, merged with #7107) against v23 :219-247. The rest of utils.cpp differs only cosmetically. Two v24 changes are unreachable or equivalent:
- the null `GetAncestor` → `break`;
- v24 reads the previous cycle through `pWorkBlockIndex->GetAncestor` where v23 used the cycle base. These are equal because every rotated `dkgInterval` is greater than 8.

---

## Findings

### M1 (major): the `ScanQuorums` continuation divergence also reaches P2P selection, not only RPC

*Design §3.8.3 "Reach", Q2, quirk Q-LQ1-1. Core: v23 quorumsman.cpp:241-305 / v24 :208-283.*

**The design's claim.** Continuation is reached only when `count > keepOldConnections`.

**What Core does.**
- On a cache hit with `cached.size() < nCountRequested`, Core continues from `back()->base->pprev`. This happens for **any** count, including `count <= keep`.
- The cached list is `min(fresh_len, keep)`, where the fresh scan asked for `max(count, keep)`. So when `count <= keep`, continuation happens exactly when fewer than `count` commitments of the type are mined at or below the store block.
- For rotated types the fresh list is then exhaustive. The continuation scan from `back().base - 1` therefore returns commitments the list already holds, namely any index whose oldest commitment predates `back()`'s cycle, and appends them as **duplicates**.
- `unordered_lru_cache::emplace` **overwrites** an existing entry (`unordered_lru_cache.h` `_emplace`). The padded vector therefore replaces the cache entry, and every later call at that store block sees it.
- Non-rotated types are unaffected in either direction. Their continuation finds nothing that the fresh scan did not.

**Who reaches it with `count <= keep`.**
- The connection-management and key-cleanup scans (`keepOldConnections`, and `keepOldKeys − held`, both of which equal keep for rotated types). A second scan at the same store block is enough.
- `quorum list` with its default count N.
- **`SelectQuorumForSigning` (count N)** for IS and recovered-signature verification.

**Observable effect.** The duplicates inflate `quorums.size()`, which feeds the `signer > quorums.size()` guard (quirk 30b). My model (`sim/scan_guard.py`, 60_75 shape) has indices 0–14 mined in an old cycle and index 20 mined in two recent cycles:
- a fresh scan returns 17 quorums, signer 20 → `None`;
- after three keep-count scans at the same store block, it returns `20@latest`.

So a fresh-cache and a warm-cache Core disagree on whether an ISLOCK or recsig verifies (`NoQuorum` against `Valid`). This is a P2P path, and through IS state it feeds `conflict-tx-lock`. `sim/scan_cont.py` shows the duplicate padding for TEST_DIP0024 with count = keep = 4.

**Where it is reachable.**
- Any rotated chain with fewer than `count` commitments under the store block. That means regtest and devnet early in their life. For selection specifically, N must be at least 3, so 60_75; that rules out TEST_DIP0024 and DEVNET_DIP0024, where N = 2 makes the selection effect impossible.
- Historically, the first DIP24 cycles on mainnet and testnet.
- Today it is unreachable at live stores on mainnet and testnet, which have at least 64 commitments.

**Not ConnectBlock consensus on any configurable network.**
- The CL type cannot be rotated.
- Asset-unlock checks are membership tests, and duplicates do not change membership. v24 also uses the cache-free `chain` overload there (assetlocktx.cpp:170).
- MNHF verification uses an explicit quorum hash.

**The precedent is miscited.** Q2 calls this "the same class as O17 DKG-08". DKG-08 was **not** declared a deviation. O17 modelled v23's caches as state, under the policy at O17-pin-delta.md:118 and :189.

**Fix.**
1. Correct the reach analysis in §3.8.3, Q2 and Q-LQ1-1, and the 30c paragraph in §3.9.
2. Preferred: follow O17's DKG-08 policy. LQ1 exposes a pure continuation entry point, e.g. `scan_quorums_continue(cached: &[ScannedQuorum], count)`, which ports `pIndexScanCommitments = back.base.prev`. QM1 owns a runtime scan cache with Core's semantics: keyed by the store-block hash, keep-bounded, overwritten on emplace, sized `max_cycles(keep) × (windowEnd − windowStart)` with Core's truncation. P2P and RPC are then bit-for-bit given the same process history. Differential tests compare against a reference that has been warmed the same way.
3. If the manager still prefers a deviation, declare it as "P2P-reachable on rotated chains with fewer than `count` mined commitments under the store block, N ≥ 3". The DIFF-RPC/O14 rule then has to exclude those store blocks, not just "counts ≤ keep", because default `quorum list` and selection are affected too.

### M2 (major): the coinbase-CL profile row is scoped to the modifier, but Core's predicate is shared

*Design §3.3, §3.13, Q1, §2.3 `UngatedRules.cbtx_cl_modifier_strict`.*

**The delta is real.** v24 `GetNonNullCoinbaseChainlock(block, height)` adds `nHeight == height` and `bestCLHeightDiff < height`. The other added checks are redundant for stored blocks: `vtx.empty`, `IsCoinBase`, `nType`, and `nVersion ≥ CLSIG_AND_BALANCE`. The design's reachability argument is right:
- Neither check can fire on a CbTx that passed full validation: `CheckCbTx` height (v23 cbtx.cpp:29-31) and EVO-18's range rule (v24 specialtxman.cpp:183-185, v24-all).
- Both are skipped only when `fCheckCbTxMerkleRoots == false`. That happens under assumevalid (`fScriptChecks`, v24 validation.cpp:2542-2568) and in `RollforwardBlock` (v24 :5197, which replays blocks that were already validated).
- So the delta needs a crafted chain under an explicit `-assumevalid`.

**My ruling on Q1: register a profile row (DEC-37).** It is cheap and turns an argument into a profile. But the row is mis-scoped. At **both** pins, `GetNonNullCoinbaseChainlock` has five consumers:

| Consumer | Kind |
|---|---|
| the modifier (llmq/utils.cpp) | consensus |
| `CheckCbTxBestChainlock`'s previous-block rule (v23 specialtxman.cpp:59 / v24 :166): `bad-cbtx-null-clsig`, `bad-cbtx-older-clsig` | consensus |
| `CreateNewBlock` (node/miner.cpp v23 :139 / v24 :131) | miner |
| `smldiff.cpp:98-99`, the `quorumsCLSigs` in mnlistdiff/qrinfo | P2P wire |
| `getrawtransaction`/`rawtransaction.cpp` | RPC |

On the same crafted assumevalid chain, the first fully validated block after an unchecked work block can be accepted by v24 and rejected by v23 through `CheckCbTxBestChainlock`. A field named for the modifier and read only by LQ1 lets LQ1 and EV4/LK1a/MN1/P6b implement two different predicates, where Core has one function.

**The "effective delta" also depends on the CbTx codec.** v24's `CCbTx` reads `merkleRootAssetUnlocks` for `nVersion ≥ 4`; v23 sees those bytes as trailing and rejects them. That is O17 EVO-06 (`cbtx_v4_known`). The payload `coinbase_cl` decodes must therefore go through BS6a's decoder under `ctx.rules`, not under a LQ1-private decode. Otherwise a crafted unchecked v4 CbTx before V24 activation diverges.

**Fix.**
- Register one row for the predicate, for example `cbtx_nonnull_cl_strict` (V23_1: false, V24_0: true), with owner EV4 or BS6a's cbtx module. Expose one pure `nonnull_coinbase_chainlock(facts, height, rules) -> Option<(Sig, u32)>`. Consumers: LQ1, EV4, LK1a, MN1, P6b and the RPC.
- Cross-reference EVO-06 and EVO-18 in the row.
- §2.3 states that the decode uses BS6a with `ctx.rules` and `ctx.bls`.
- Add an L2 invariant: every mainnet and testnet CbTx with v ≥ 3 has `nHeight == h` and `bestCLHeightDiff < h`. This proves the history claim instead of arguing it.

### m1 (minor): the rationale for quirk 30b is wrong, and §3.9 contradicts itself

*§3.9, §3.11 30b and 30c.*

`signer > quorums.size()` compares the signer with the **number of scanned quorums**, not with the index range. When fewer than N commitments are scanned, which happens in early cycles and on young chains, an index that exists is rejected. With a fresh scan of 17 quorums that contains index 20, signer 20 gets `None` (`sim/scan_guard.py`). The guard is therefore not "off by one but harmless".

§3.9 also says a never-mined index resolves to `None` "unless a cycle-1 entry with that index was swept in". That case cannot happen: a never-mined index has no entries in any cycle.

The literal port is correct. Fix the text and add a constructed vector (L0 quirk-30 set) in which the guard rejects a present index.

### m2 (minor): `log2` evidence gaps

*§3.10.2–3.10.3, the appendix.*
- **Missing win64 listings.** `fp/listings/` holds 15 call-site listings. The three win64 sites, which the table claims as `cvtsi2sd`/`cvttsd2si`, were located by `find_win_log2.py`'s pattern search, but its output was not kept. Archive it, so the "18 sites" claim is reproducible.
- **glibc coverage for count 2.** Linux builds bind `log2@GLIBC_2.29`, so they run on any glibc from 2.29 on. Count 32 is proven for all of those by the 0.547-ulp bound. Count 2 (1.0 sits on a binade boundary) was measured only on 2.31, 2.36 and 2.43. Bridge the gap by showing that `e_log2.c` and `e_log2_data.c`, and the x86_64 FMA/generic ifunc wrappers, are unchanged from 2.29 to 2.43, or measure the intermediate versions in containers. This affects only regtest and devnet.
- **Verdict on Q4:** equivalent, conditional on Q6 for macOS count 2 and on this bridge.

### m3 (minor): the reconstructed quarter differs from the original new quarter for pre-V19 cycles

*§3.6.2, §3.11 28d, the LQ1b-D acceptance criterion in §7.*

With `skipRemovedMNs == false`, the encoder's combined list includes removed masternodes in its "used" tail. `BuildQuorumSnapshot`'s bitset spans only `CalculateQuorum(allMns)`, so the decoder's combined list (v24 :199-223) cannot contain them. The skip indices then address a list with a different shape. This is a second cause of "a reconstruction can differ" alongside 28f, and it affects mainnet cycles 1,737,792 … 1,898,784.

The LQ1b-D acceptance criterion ("reconstruction … equals the members of `quorum info` for the H−C … H−3C quarters") must therefore compare against the quarters **embedded in the later quorums**, not against the original quorum's new quarter. State this in 28d and in the acceptance criterion.

### m4 (minor): `select_start_height` signature

*§2.5.*

Core takes `int signHeight`: −1 means the tip, any other negative value gives `nullptr`, and `signHeight − offset` may go negative. `Option<u32>` pushes that mapping onto every caller, including LK1's P2P-derived heights. Take `i32` with Core's semantics, or document the caller contract and use `checked_sub`.

### m5 (minor): the decoder termination bound

*§6.5.*

Each decoder step either pushes a member (at most `N·size/4` in total) or consumes a skip entry. The bound is therefore `N·size/4 + skip_list.len()`, not "plus the list length". A skip entry that never matches is never consumed, and the loop still terminates.

### n1 (nit): §3.7.1 "Not reached"

Add two items to the "Not reached" list:
- **Default regtest.** `DIP0024Height = 1` and `DIP0003Height = 432`, so cycle bases 24 … 408 are rotation-enabled, but `ProcessBlock` returns before PreCompute. No row is written for them, and none could be, because the list is empty (28e-1). Every FT chain crosses this, so it deserves a constructed vector.
- **v24 dual chainstate.** rc.3's assumeutxo path (`ActivateSnapshot`, `EvoDbIdentity::SNAPSHOT`, a regtest-only `m_assumeutxo_data`) shares the EvoDB. It is out of scope under NG-6, but say so.

### n2 (nit): constraints on the oracle shim (§6.2)

- `GetAllQuorumMembers`' members caches are function-level statics, which is process-global.
- `IsQuorumRotationEnabled` and `GetNonNullCoinbaseChainlock` read the global `Params()`, so the shim gets one chain per process.
- `GetSnapshotForBlock` reads the 32-entry LRU before the DB, so rows must be seeded through `StoreSnapshotForBlock` or with a fresh `CQuorumSnapshotManager`.

### n3 (nit): §3.8.3 cache mechanics

If M1 is resolved by modelling the cache, state the overwrite-on-emplace semantics and the LRU size and truncation policy (`unordered_lru_cache`, truncation at 2× max). Eviction decides whether continuation happens, so it is part of the behaviour.

---

## Answers to the brief

1. **Ordering, tie-breaks, modifiers, skip modes, and the author's corrections:** confirmed at both pins (see "What I verified").
2. **No-snapshot paths and the emission boundary:** both no-snapshot paths and the boundary are confirmed. I found no missed write path at either pin. See n1 for default regtest and the dual chainstate.
3. **Pin delta and `cbtx_cl_modifier_strict`:** the delta is confirmed. Register a row, but scoped to the shared predicate and tied to EVO-06 and EVO-18 (M2).
4. **`log2`:** provably equivalent for count 32 everywhere and for count 2 on the measured Linux and Windows libms. Overrides cannot change the domain. Conditions are in m2 and Q6.
5. **`ScanQuorums` divergence:** it is not RPC-only. It reaches P2P selection (IS and recsig verification) and connection/key management on young rotated chains, through IS state too. It does not reach ConnectBlock-deterministic consensus on any configurable network (M1).

## Verdict

**APPROVE-WITH-CHANGES.** Resolve M1 and M2 in the design text and in Q1/Q2 before LQ1a-C and LQ1a-B start. The minors can be fixed in r1 without another full review round.
