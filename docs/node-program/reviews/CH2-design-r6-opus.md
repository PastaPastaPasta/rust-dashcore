# CH2 design rev4 — Opus round 6 (sequential reviewer after Sol r5)

**Verdict: CHANGES-REQUIRED.** There are four majors, and each is small to fix in the text. None of them reopens DEC-95, DEC-99 or DEC-100. Two would make a PRD-gated behaviour fail by construction:
- `feature_dbcrash` cannot pass under the asynchronous flush as written (O-R6-M1).
- The D12 shadow's Core flush epochs are tied to *our* flush-policy state (O-R6-M2). That makes `HaveCoinInCache`, and with it P2P orphan handling, diverge from Core on ordinary long-running nodes.

The other two are a2 acceptance criteria that cannot be met or are undefined as written (O-R6-M3, O-R6-M4). CH2a is implementing a2 next, so these need text before a2 starts. a1 (already on `node/ch2a-coins`) is affected only by a minor (O-R6-m4).

Scope: design blob `593d95e3` (= `28bff6ce`, rev4), read in full. Core is `~/workspace/dash` at `v24.0.0-rc.3` (`0364ed62`), and Core lines below are `src/` at that tag unless the path says otherwise. I read the r5 Sol verdict and its two nits (G-R5-n1, G-R5-n2) and do not repeat them. I looked at `node/ch2a-coins` (`d3e560dc`, a1 done: the fallback ships) only to see how the text is being read. Nothing was modified or committed.

---

## Majors

### O-R6-M1 — `feature_dbcrash` fails by construction under the asynchronous flush

**Where:** §1.2 (US-NODE-04: "`feature_dbcrash` passes"), §4.6 and §3.5.1 (the `-dbcrashratio` rows), §6.6 (CH5 runs it), and §4.2 (V continues while W flushes).

**What the test assumes.** `feature_dbcrash.py` (test/functional, v24) does the following:
- Its nodes are left **unconnected** (`:74`). Blocks are delivered only by `submitblock`.
- When a `submitblock` fails because the node died, `restart_node(i, block_hash)` restarts it and calls `waitforblock(expected_tip)` with `expected_tip` = the block whose submission failed (`:83-103`, `:143-155`). It gives up after 120 s and fails the test.

This only works because Core's crash point is synchronous with the tip. `CCoinsViewDB::BatchWrite` rolls `-dbcrashratio` only after a partial batch (`txdb.cpp:152-163`). That happens only inside `FlushStateToDisk`, on the thread that is connecting the block being submitted. By then `WriteBlockIndexDB` has durably written **every** accepted block's index entry (`validation.cpp:3064`, before `CoinsTip().Flush()` at `:3091`). The coins marker's `new` is the current tip. So after a restart, `ReplayBlocks` lands exactly on `expected_tip`.

**What CH2 does.** V freezes at X, `submitblock(X)` returns, and V goes on to accept and connect X+1, X+2, …. Meanwhile W writes F and rolls `-dbcrashratio` on its sub-batches. Their index records are dirty in memory: they reach disk only in the *next* freeze's B1, or in an hourly index-only write.

**Scenario.** On node0 (`-dbcache=4`, `-dbcrashratio=8`):
1. V freezes at X during `submitblock(X)`, which returns.
2. `submitblock(X+1)` is accepted and connected by V.
3. W's third sub-batch of F (new = X) rolls the crash and calls `_Exit(0)`.
4. `submitblock(X+2)` fails, so the test restarts with `expected_tip = X+2`.
5. Recovery brings coins to X. Neither X+1 nor X+2 has a durable index entry, and no peer will re-send them, so `waitforblock(X+2)` never completes. The test fails ("Unable to successfully restart node 0").

Whenever W lags V at the moment of the roll, the test fails. Under the design's normal operation that is most of the time.

ST3a's CRASH model does not catch this. The state after the crash is consistent; what breaks is the FT's expectation that the tip after recovery is at least the last block submitted.

**Fix (pick one and declare it).**
- **(a) Recommended.** When `-dbcrashratio > 0` (debug-only in Core), every freeze is followed at its call site by V waiting for `FlushDone`. With V never ahead of the flush that rolls, the crash leaves `new` = the block being submitted, as in Core. ST3a's V-ahead crash coverage (C6, C9) stays the evidence for the asynchronous path. Declare it as a † row or a D-row ("test hook: synchronous flush under `-dbcrashratio`").
- **(b)** Make accepted blocks' index records durable before `submitblock` returns. This is more invasive and costs an fsync per block.

Either way, add a CH5 acceptance line stating that `feature_dbcrash` runs against that mode, and a b1 regtest smoke that reproduces the scenario above.

### O-R6-M2 — The shadow's Core flush epochs need Core's own `FlushStateToDisk` state, not `FlushPolicy`'s

**Where:** §2.4.5 (lines 700-702: "`FlushPolicy::decide` already evaluates Core's trigger table … Core's flush causes are ALWAYS, `fPeriodicFlush` (24 h), `fFlushForPrune`, `fEvoDbCacheCritical` …"), §2.4.5 line 709 ("ALWAYS, 24 h, prune and evo flush points are exact"), §3.3 timers (line 1474: `last_flush` "advance[s] … at the freeze"), and §3.3's prune deferral (line 1467).

Several of Core's flush causes depend on state that differs between Core and CH2:
- **24 h.** `fPeriodicFlush` compares against `m_last_flush`. Core resets it only at its own full flushes (`validation.cpp:3100`), after initialising it on the first call (`:3031-3033`). `FlushPolicy.last_flush` is reset at **every freeze of ours**: THRESHOLD, LEFT_IBD, DMAX, EMPTY and SCRIPTED. At the tip, the 24 h flush is Core's main epoch, because LARGE is rarely reached there. A shadow that reads `FlushPolicy`'s `PERIODIC_FLUSH` therefore flushes at (our last freeze + 24 h), not at (Core's last flush + 24 h).
- **Prune.** In prune mode, Core decides `fFlushForPrune` at **every** `FlushStateToDisk` call, including the NONE call after `AcceptBlock` (`:4855`). It uses:
  - its **in-memory** prune locks, which are the indexes' best blocks (`index/base.cpp:437-444`), not durable locators;
  - its own `m_check_for_pruning`, which it clears at the decision (`:3017`).

  CH2 evaluates with `predicted_locks` and, while F is in flight, **defers the decision and keeps the check armed** (§3.3). A shadow that follows our PRUNE cause misses Core's prune flush whenever F is in flight. It also never evaluates prune at NONE sites, because §2.4.5 says the shadow evaluates "at the same call sites" only for CRITICAL and LARGE.
- **Evo.** `fEvoDbCacheCritical` is `m_evoDb.GetMemoryUsage()`, measured since Core's last `CommitRootTransaction` (`:3096`), i.e. since Core's epoch. Our `EVO_CRITICAL` uses `StateDelta::bytes()` of active's window. The text names "CH7a's meter of Core's EvoDB usage" but does not say it is measured since the Core epoch.
- **Missing ALWAYS source.** `PeriodicStats` (`init.cpp:921-928`, with `-statshost`) calls `ForceFlushStateToDisk()` from the scheduler on every stats period. §3.3's ALWAYS list (shutdown, copy points, cache shrink) omits it. If rdashd implements statsd, this is an ALWAYS cause and a Core epoch. If it does not, say so.

**Why this is a major.** The ruling allows no named deviation for `HaveCoinInCache`, and the answer is peer-visible:
- `TX_MISSING_INPUTS` sends a transaction down the orphan path and requests its parents (`net_processing.cpp:4963`).
- `TX_CONFLICT` (`txn-already-known`) does not.

**Scenario.**
1. A node finishes IBD. Our LEFT_IBD freeze resets `FlushPolicy.last_flush`; Core's `m_last_flush` is unchanged.
2. 24 h after Core's last flush, Core empties its cache, so tx T mined before then is no longer a member.
3. Our shadow keeps T's outputs as members until 24 h after our freeze.
4. During that gap, a peer relays T again with a spent input. Core answers `TX_MISSING_INPUTS` and requests parents; we answer `TX_CONFLICT` and do not.

**Fix.** Give the shadow its own pure **`CoreFlushModel`**, evaluated at **every** Core call site (NONE, IF_NEEDED, PERIODIC, ALWAYS) regardless of F or our deferrals. It holds Core's state:
- `core_last_flush`, `core_last_write` (for the "first call" rule);
- `core_check_for_pruning` (set where Core sets it, on a new block file in prune mode; cleared at Core's decision);
- a Core-model prune dry run over Core's in-memory locks;
- the evo usage since the Core epoch.

It returns "Core does a full flush here", and only `on_core_flush` consumes that. `FlushPolicy` keeps its own state for physical freezes. a2 adds two unit tests: a 24 h epoch under interleaved own-cause freezes, and an automatic-prune epoch with F in flight.

### O-R6-M3 — "meter == Core's `DynamicMemoryUsage()` after every op" is undefined for a block's `BatchWrite`

**Where:** §2.4.5 lines 704-708 (chunks as "a function of the peak node count") and 711-720 (`core_meter_vectors`: equality "after **every** op"), §7.1 a2, and §6.2/c3 (`diff_coins_ops` compares `DynamicMemoryUsage` per op over fuzzed streams).

**Core's number depends on an iteration order nobody models.**
- `CCoinsViewCache::BatchWrite` walks the **child's** `cacheCoins` in its hash-iteration order (`coins.cpp:182-200`). The child is a fresh `CCoinsMap`, and production salts are random per map instance: `SaltedOutpointHasher` takes `GetRand` unless `deterministic` is set (`util/hasher.cpp:14-17`; `coins.cpp:37`).
- In a connect, that walk interleaves two kinds of step:
  - **inserts** into the tip: new outputs, via `cacheCoins[it->first]`;
  - **erases** of tip-FRESH entries spent by the block: the common IBD case of spending recent coins, `:225-229`.
- `PoolResource` serves freed nodes from its free list first and carves a new 256 KiB chunk only when the free list is empty (`support/allocators/pool.h:225-231`). Chunks are never returned before `ReallocateCache`.
- So the chunk count after the block depends on the largest running count of (inserts − erases) over that order, not on the node count.
- The same holds for the bucket array. libstdc++ rehashes on insert and never shrinks on erase, so the bucket count after the block depends on the largest element count reached during the walk.

**Magnitude.**
- Chunks: up to one block's FRESH-erase count of nodes, rounded to 256 KiB.
- Buckets: when the tip's element count is near a rehash point, the whole step. The 1,447,153 → 2,938,679 step that §2.3 quotes is +11.4 MiB of accounted usage, ≈ 4.5 % of B_DEC43. Whether it happens at block k or k+1 depends on Core's random salt.
- Core's own LARGE or CRITICAL point, and therefore its `HaveCoinInCache` epoch, can therefore move by a block between two Core runs on the same chain.

**Consequences.**
- a2's vectors can pass only if the probe uses deterministic salts **and** the meter reproduces libstdc++'s iteration order for SipHash over those salts. The design does not specify that, and it is not worth doing. Sol's r4/r5 spot checks used insert-only or sequential scripts, which never exercise this.
- c3 against O2a-CO will flake on mixed blocks unless the oracle runs deterministic and the meter replays the oracle's order.

**Fix.**
1. State it plainly: Core's `DynamicMemoryUsage` after a mixed `BatchWrite` is salt-dependent. "Exact" therefore means exact for a given BatchWrite order.
2. Define the meter's canonical order for production. Recommended: apply the erases before the inserts, which is the lower bound and keeps the meter monotone in the same direction as Core's minimum. Declare the ±1-block epoch band as Core's own nondeterminism, not a CH2 deviation.
3. In tests, have the pinned probe (a2) and O2a-CO (c3) **report the BatchWrite order they used** (or the node counts at each step), and have the meter replay it. Equality after every op then holds exactly.
4. Add an a2 script with a mixed block (outputs plus spends of tip-FRESH coins) sized to straddle a chunk boundary and a bucket rehash point.

### O-R6-M4 — `have_in_cache(&self, op) -> bool` cannot be implemented as specified

**Where:** §2.4.1 line 545, §2.4.5 lines 680-681 and 694-699 (`member(op, coin)`, "given op's coin as V sees it (active → F → **DB**)"), and §7.1 a2 (the `mempool_accept.py` case: "mine, force an own-cause freeze and release, `testmempoolaccept` → `txn-already-known`").

**The problem.** A coin created after Core's epoch but before our last release lives **only in the DB** on our side. It is still a member in Core, because Core's map holds it DIRTY. DEC-43 has our flushes at about 2× Core's count, so this is the normal case, not an edge. To answer, `member` needs that coin's height and its spent status from the DB, and a DB read can fail. The declared signature has `&self` and returns a bare `bool`, so it has nowhere to put a `CoinsReadError`. That contradicts §2.4.4's rule that a read error is never a verdict. a2's own `mempool_accept.py` case takes exactly this path.

**Cost Core does not pay.** ATMP calls `HaveCoinInCache` once per prevout (`validation.cpp:963`) and, when an input is missing, once per output of the transaction (`:974`). Core's calls are hash lookups. A literal port is one RocksDB `get` per output. A peer sending orphans with many outputs therefore gets n engine reads per transaction where Core does n hash probes.

**Fix.**
1. Change the signature to `have_in_cache(&mut self, op) -> Result<bool, CoinsReadError>`, as a V command (ruling 9).
2. For ATMP's prevouts, compute membership from the shadow's state **before** `on_fetch`, using the coin that the following fetch returns. This is one read, and it serves both Core calls (`:963` then `m_view.HaveCoin`).
3. For the outputs loop at `:974`, use **one prefix seek on the txid** over `coins` (keys are `txid ‖ VARINT(vout)`) after checking active and F. All outputs of a transaction share its height, so one seek answers "any unspent output of txid at a member height". Membership through `fetched_unspent` or `restored` stays a set lookup.
4. State that neither path inserts into our layers.

---

## Minors

### O-R6-m1 — The shadow's event sources are incomplete; hook them structurally

§2.4.5 lists Core's fetches as "connect inputs, ATMP prevouts, `gettxout`", and §2.11 MP1 lists Uncache at `:1744, 1775`. At v24 these Core paths also call `FetchCoin` on `CoinsTip()`, so they change both membership and the meter:
- `GetUTXOCoin`, `GetUTXOHeight`, `GetUTXOConfirmations` (`validation.cpp:326-347`), used by `rpc/evo.cpp:1584,1605`, `rpc/masternode.cpp:679` and `init.cpp:2491`;
- `evo/providertx_service.cpp:128`;
- InstantSend's `CheckSpecialTx` on `CoinsTip()` (`instantsend/signing.cpp:215`);
- CoinJoin's `CCoinsViewMemPool` over the tip (`coinjoin/coinjoin.cpp:374, 477`);
- wallet `findCoins` (`node/coin.cpp:17`);
- `BroadcastTransaction`'s `AccessCoin` of every output (`node/transaction.cpp:51`);
- `TestBlockValidity`'s child over the tip (`viewNew` in `validation.cpp`), reached from `getblocktemplate` and from `generate` through `CreateNewBlock`;
- REST `getutxos` (`rest.cpp:917`);
- `gettxoutproof` (`rpc/txoutproof.cpp:77`);
- `verifychain`, whose CoinsTip-based `VerifyDB` (`rpc/blockchain.cpp:1440`) runs a child over the tip at level ≥ 3;
- two more uncache and membership sites: `LimitMempoolSize`'s Uncache (`:404`) and the package path's `HaveCoinInCache` (`:1591`).

**Scenario.** Tx T is mined before Core's last flush. `sendrawtransaction(T)` returns `ALREADY_IN_CHAIN` and, in doing so, caches T's outputs in Core. A later `testmempoolaccept(T)` then gets `txn-already-known` from Core, while a shadow that missed the `BroadcastTransaction` fetch answers `bad-txns-inputs-missingorspent`.

**Fix.** Call the shadow **inside** `CoinsCache::fetch` for every successful lookup, including active and F hits, and let the shadow decide "already in Core's map". Then every Core `CoinsTip()` reader that is ported as a V `fetch` is covered by construction. Keep a table of the Core call sites above and the task that owns each one.

### O-R6-m2 — Copy-point snapshots must be atomic with respect to the next flush

**Where:** §2.6 "Copy points" (line 953) and §2.5 "Release accounting" (line 824).

Core's `dumptxoutset` holds `cs_main` across `ForceFlushStateToDisk()` and cursor creation (`rpc/blockchain.cpp:3165-3180`). `gettxoutsetinfo` reads `GetBestBlock()` under `cs_main` right after its flush (`:1238-1246`).

The design says "iterate an engine snapshot taken after Bk returned" but does not say who takes it or what stops the next B1 first.

**Scenario.**
1. A copy-point ALWAYS flush returns.
2. V immediately freezes on a pending cause (CRITICAL, DMAX, EMPTY or PRUNE).
3. W writes the next B1 in split mode: marker on, `meta.tip` erased.
4. The RPC thread then takes its snapshot. `gettxoutsetinfo` or `dumptxoutset` reads a transition state, gets a wrong hash or partial contents, and reports no best block.

**Fix.** The ALWAYS command returns a snapshot taken on V before V processes anything else (or by W between Bk and `FlushDone`). The RPC reports `meta.tip` read from that snapshot and asserts that the snapshot has no marker. Add an a3 or b1 test that holds a pending cause across a copy point.

### O-R6-m3 — Contradiction: does the child cache its inputs?

The text says both:
- "caching it in the tip **and the child** as Core's FetchCoin does" (§2.4.2, lines 582-583), and "its `have_inputs` caches each input in the tip and the child" (§3.2, line 1450);
- "Core's child also takes a copy, **ours does not**" (§2.5 Ownership, line 812).

This decides `spend`'s ownership branch, the child's memory and the ownership witnesses.

**Fix.** Keep §2.5's rule (the child holds no input copies; reads resolve through active) and correct §2.4.2 and §3.2.

### O-R6-m4 — a1's "no shard grows while no F is in flight on the mainnet trace" is false at small budgets; the floor margin is thin

**Where:** §2.3 lines 468-470 and §7.1 a1 ("on the mainnet-shaped sweep no shard grows while no F is in flight").

The trigger's margin is 1/64 of a shard's buckets.

**Arithmetic.**
- At B = 4 MiB the presize is ≈ 25,800 buckets, so shards have 256 or 512 buckets and margins of **4 or 8 entries**.
- A 2017-era mainnet block adds thousands of entries, i.e. tens per shard.
- The safe per-block size scales with B: ≈ 20,000 × B / 252.75 MiB. That is ≈ 5,000 entries at 64 MiB and ≈ 320 at 4 MiB.

So the assertion fails at the low end of the sweep for any reading of the rule, which is what PF1's own note already reports for rev3 (`grew_before_freeze` at every budget).

**Fix.** Assert no growth at B_DEC43 only, as a counted check: report growth events per budget, and gate zero at B_DEC43. At small B the trigger is pure density cost, and saying so is enough.

**Related.** PF1 measured the fallback's floor margin at 4 MiB as **1.044 to 1.054** (`d3e560dc`). The rev4 trigger takes about 1/56 off that, leaving ≈ 1.025 to 1.035. It still clears, but a1 must re-run the floor with the trigger, and §5.5's "density between parity (fallback) and ≈ 1.45× (arena)" should now read ≈ 1.17× at DEC-43 (2.58 M against 2.20 M).

### O-R6-m5 — a2's acceptance relies on CH2b (held) components

a2 tests "`FlushPolicy::decide` maps the state to `CACHE_LARGE`/`CACHE_CRITICAL` …", Sol's divergence case ("THRESHOLD still freezes"), and the shadow's epochs at Core's points. But `FlushPolicy` is b1 content (§7.2), and CH2b is held for SB0 (DEC-81).

**Fix.** Name a CH2a home:
- `core_size_arith`, the shadow and O-R6-M2's `CoreFlushModel`, all pure;
- a minimal THRESHOLD/CRITICAL boundary check in `CoinsCache`.

b1's `FlushPolicy` then consumes these. Otherwise a2 waits on SB0 or invents a stub that b1 throws away.

### O-R6-m6 — A grown frozen layer plus a fresh presize makes V stall twice

**Where:** §2.3 lines 465 and 483-484.

The new active's presize is `f·(B − R − B/16)` whatever F's size. When the fallback's shards have doubled (F in flight at the trigger, or the worst single block), freezing that layer leaves F at about 2× presize. Adding a new presize pushes Σ past B, so native CRITICAL makes V wait for the whole of that F right after it has already waited for the previous one. The text acknowledges the wait, but there is a cheap fix.

**Fix.** Presize the new active at `min(f·(B − R − B/16), B − R − B/16 − F.allocated_bytes())`. It can still grow (DEC-95). a1 reports the stall count with and without the change.

### O-R6-m7 — §10.1 is missing one requirement and holds items that are not SB0's

- **Missing (P10).** Block-index records and file infos are persisted through CH2's **B1, in the chainstate engine, in the same batch as `meta`, the marker and evo** (I1, †2). Recovery also loads `IndexAccess` from that durable state before V starts (§4.3 steps 4-6). If SB0 moves headers or the block index to a separate store, which an SPV header store invites, I1 and †2 fail and recovery's checks on `new` have no atomic basis.
- **Producer or CH2?** F4's "mirrored in the hub" and F7's publication after B1 are CH2-side obligations (the hub and W), not producer guarantees. Label each F as *producer* or *CH2*, so SB0 knows what it must implement.
- **Deregistration.** Add an F: once `deregister` returns, the producer enqueues nothing more, and V's backpressure never waits on that queue (the shutdown order in §4.2 relies on this).
- **P9** (mempool coin access on V) is a node-internal rule. Keep it, but mark it "if SB0 shares the mempool".

### O-R6-m8 — The fallback's shard routing must use hash bits that hashbrown does not

**Where:** §2.3 ("a key's shard is the bucket-weighted slot of its salted hash").

hashbrown takes the bucket index from the low bits and the 7-bit h2 tag from the **top** bits of the same hash. If routing uses the top bits (the natural `(h as u128 * total) >> 64`), every key in a shard shares nearly the same h2. The SIMD tag filter then stops filtering, and each probe compares several 36-byte keys.

The branch already routes on bits 25..56 (`sharded.rs:94`). The design should state this as a requirement so that a later "simplification" does not regress it, and a1's PMU line should cover it.

---

## Nits

- **n1.** `Usage` has no field for the shadow's sets, which §2.4.5 line 723 counts in native Σ, and §2.5 line 793's "native Σ = active + F + prefetch + R" omits them. Add `shadow: u64`, or say they are in `active`.
- **n2.** `LongRef`'s arena variant (§2.2) is obsolete now that the arena is deleted. `held_out` must be a shared atomic, because `LedgerBox` drops on any thread. Say both.
- **n3.** `&dyn EngineTuning` cannot be written while the trait has `type E` unless it is `dyn EngineTuning<E = …>`. Say `T: EngineTuning` throughout.
- **n4.** The F table lists F10 before F9.
- **n5.** `wipe_chainstate` and its resume (§2.6; §4.3 step 2) are covered only by an a3 unit test. Add a C-family (C12) so that both crash kinds and nested crashes are swept.
- **n6.** The tombstone rebuild is O(shard) on V inside a block. Near the trigger that is at most about one rebuild per shard per block (≤ 64 × ~1 ms at DEC-43 sizes). a1 should report **per-block rebuild time at p99**, not only frequency, against connect-latency metric #5.

---

## Answers to the brief's five questions

1. **Correctness against Core at v24.**
   - **`HaveCoinInCache`:** O-R6-M2, M3, M4 and m1. These are the only Core-visible gaps found, and none touches consensus.
   - **`-dbcrashratio` / `feature_dbcrash`:** O-R6-M1. The roll points (after partial batches only, in flushes and in recovery) match `txdb.cpp:152-163`.
   - **`getindexinfo`, `gettxoutsetinfo`, prune and index interplay:** these hold, except the copy-point atomicity in m2. The capped eager commit only raises L, so DEC-64 Q10 cannot recur.
   - **`invalidateblock` / `reconsiderblock`:** consistent with v24's `BlockDisconnected` → `Rewind` (`index/base.cpp:308-330`, `:238-259`) and the restored-coin FRESH rules (`coins.cpp:191-247`).
   - **Shutdown order:** matches `init.cpp:383/453`.
   - **Error strings:** re-checked `coins.cpp:364/369`, `init.cpp:2089`, `shutdown.cpp:27-37` and `validation.cpp:3051/3089`.
2. **Crash consistency.** ST3a's sweep crashes at every store event of `ChainSut` and `CoinsSut`, so every store-call boundary of the flush machine and of recovery is covered by construction. The C-families name the PLAN items, and the non-store steps (publish, release) have no durable effect. Gaps:
   - the wipe (n5);
   - `RocksTuning`'s `after_bk` and open-time paths, which the seam replaces with `MemoryTuning` (acceptable under DEC-62);
   - the FT-level property in M1, which is not a consistency property.
3. **Implementability of CH2a.** a1 is done and was read correctly, apart from m4 and m8. a2 cannot be built from the text without guessing on M3, M4, m1, m3 and m5. The list below is what the implementer must get right.
4. **Performance.**
   - The kept fallback gives ≈ 1.17× Core's resident coins at DEC-43 (PF1). L1 cannot carry the 2× target, and §5.6's Amdahl paragraph should say so.
   - Risks: m6 (double stall), n6 (rebuild latency), M4's per-output reads, and the trigger's density cost (≈ 1/56 plus the fullest-shard noise already present in PF1's `grow`), which buys nothing at small B (m4).
   - Flush cost and the post-Bk memtable flush are off V. The files barrier blocks V's appends for one `fdatasync` (L6), which is acceptable.
5. **SB0 interface.** F1–F10 and P1–P9 are sufficient for the feed. They are missing P10 (block index in B1) and the deregistration guarantee, and they mix CH2-side duties into the producer list (m7). Otherwise they are minimal: each item is used by a lemma, a scenario or a planted bug.

---

## Things CH2a must get right (for `node/ch2a-coins`)

1. **`have_in_cache` reads.** It is fallible and may read the DB. Compute membership **before** `on_fetch` for ATMP prevouts, with one read. Answer the outputs loop with one txid prefix seek (M4).
2. **Shadow hooks.** Call the shadow from inside `CoinsCache::fetch` on every hit, from any layer (m1). The shadow decides "already in Core's map", not the layer that answered.
3. **Meter order.** Apply a block's `BatchWrite` to the meter in a defined order (erases, then inserts). In `core_meter_vectors`, replay the probe's reported order. Include a mixed block that straddles a chunk boundary and a rehash (M3).
4. **Core flush model.** The shadow's epochs come from a `CoreFlushModel` with Core's own 24 h clock, prune check and in-memory locks, and Core-epoch evo usage, evaluated at every call site including NONE (M2). Never from `FlushPolicy`'s timers.
5. **The child holds no input copies.** Reads resolve child → active → F → prefetch → DB, and `spend` copies anything the child does not own (m3, §2.5).
6. **Accounting at boundaries.** `held_out` is a shared atomic. `LedgerBox` can move its bytes from the child's count to `held_out` without double-counting. Assert `tagged == native Σ − R` at every op boundary.
7. **Shard routing.** Use bits that are disjoint from hashbrown's h1 low bits and its top-7 h2 bits, as the current code does at `sharded.rs:94` (m8).
8. **Shard trigger and rebuild.**
   - Trigger at 55/64 of a shard's buckets, checked at block boundaries only.
   - Rebuild at the same bucket count when `len == capacity` and the load is below the trigger. Count the rebuild in native Σ.
   - Report rebuild time per block (n6), and gate "no growth" at B_DEC43 only (m4).
9. **Fallible reads.** Every read path through `GuardedRead` / `GuardedSnapshot` trips the latch before returning `CoinsReadError`. A decode failure or trailing bytes go to `corrupt` (DEC-78, D7).
10. **Physical and Core counters stay separate.** `NativeUsage` never reaches the shadow, and `CoreModelUsage` never reaches the physical policy (DEC-99). The Sol witness (20,000 outputs at `-dbcache=4`) is the test for that.

---

Reviewed read-only. No source or design file was modified and nothing was committed. The scratch directory `/work/scratch/ch2-r6-opus/` was created, not used, and removed.
