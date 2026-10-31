# CH2 design review r2 (Opus): closure check of rev1

| | |
|---|---|
| Design | `designs/CH2-coins-flush.md` **rev1** at 989aec57 (`node/integration`). Line numbers below are rev1's |
| Reviewer | Opus 5.5, independent; author of round 1 (`reviews/CH2-design-r1-opus.md`) |
| Rulings read | DEC-77, DEC-78, DEC-80; GPT r1 for context (GPT r2 runs in parallel, not read) |
| Inputs checked | Core `src/` at v23.1.8 and v24.0.0-rc.3 (`index/{base,blockfilterindex,coinstatsindex,addressindex}.cpp`, `validation.cpp`, `dbwrapper.h`, `shutdown.cpp`, `coins.cpp`, `consensus/consensus.h`; FTs `rpc_getblockfilter.py`, `mempool_accept.py`, `feature_{address,spent,timestamp}index.py`), extracted under `/work/scratch/ch2-r2-opus/{v23,v24}`; ST3a rev5 §5.5–§6.2; CS5a `node/cs5a-coins` (4c75912b) `coins/{coin,view}.rs`; the rev1 liveness model |
| Scratch | `/work/scratch/ch2-r2-opus/`: `model/` (a copy of rev1's model plus my variants), `shard_cliff.py`, `gate_points.txt` |
| Status | Final (r2) |

## Verdict up front

**APPROVE-WITH-CHANGES, staged.** rev1 closes most of round 1: 14 of my 17 findings and all 8 nits; the other 3 are partly closed. The four mechanisms rev1 adds are sound in structure: the rewind gate, parking with compression, the `DurableBelow` view and the overshoot rule. I re-ran the rev1 model and it reproduces the claimed results.

Checking the new subscriber data path against Core turned up four new majors (R2-M1, M2, M3, M6). Two more majors are older text I missed in round 1 (R2-M4, M5).
- **R2-M1 (visibility).** Staged index rows are invisible to RPC lookups until the next flush.
- **R2-M2 (filter position).** blockfilterindex's file position moves backward, so new filters overwrite the bytes of stale filters.
- **R2-M3 (IX2).** The row classes cannot express IX2's address, spent and timestamp stores.
- **R2-M6 (deadlock).** A model variant shows a deadlock when V freezes while `Connected(new)` has not yet been emitted.

The remaining two:
- **R2-M4.** `D_max = 1,024` makes forced flushes the dominant cause in the DEC-43 cell.
- **R2-M5.** The 64-shard fallback has the unsharded table's capacity cliff, and the layout gate's sample points can pick the wrong layout.

None of them needs an architectural change. CH2a can start once R2-M5's gate metric is fixed. CH2b b1 and b2 can start as written. §2.9 and §4.4.1 need a rev2 and a reviewer's closure check before b3, because R2-M1 to M4 and R2-M6 all land there.

---

## 1. Dispositions of my round-1 items

| # | Sev | rev1 answer | Disposition | Evidence / residual |
|---|---|---|---|---|
| F1 | major | rule O, a declared `T(op)`, an accounted overshoot, release ordering | **PARTIAL** | The structure is right: it matches Core's after-connect CRITICAL check (`validation.cpp:3207 / :3457`). `T_max` is right: 2,000,000 / 9 = 222,222 outputs (`coins.cpp:342`), and 48,780 41-byte inputs. The release race is fixed. **But `T(op)` leaves out the growth of the layer that an op triggers** (R2-m1). The asserted invariant `usage ≤ max(B, U + T(op))` (`:508`) is therefore false at an index or shard doubling |
| F2 | major | `best_block()` in memory per `IndexRules.rewind_on_disconnect`; MPIX-11 in §3.8; C8(b) | **CLOSED** | v24 `BlockDisconnected` (`index/base.cpp:308-330`) only rewinds when `best ≥ h(pindex)` and pindex is on the indexed chain. §2.9 says "on the indexed chain", which matches. The v23 lazy path is `:263-271`. The durable half is a declared deviation (§2 below) |
| F3 | major | plant sites X16a/X16b; FA7 | **CLOSED** (wording) | FA7's first clause ("the operation returns `Fatal`") catches X16a, X16b and X21. Its second clause has an empty window under the seam (R2-m3) |
| F4 | major | cgroup cap with anon + page cache; gate on `rss_anon_peak` | **PARTIAL** | §5.5 is right. But the proposed CH2c PLAN row still gates on "RssAnon+RssFile" (`:1758`), and the W-MC rule still says "on RSS (anon + file)" (`:1568`) (R2-m2) |
| F5 | minor | T12, C11 and the T20/T21 connect halves moved to CH3a | **PARTIAL** | §6.4 and §7.2 move them. "Edits to existing rows" (`:1764`) changes CH3a's deps and BG but **not its acceptance**, so the move is not in any PLAN text (n1) |
| F6 | minor | `*** ` log line, the generic user message, the misc warning | **CLOSED** | Re-checked: `shutdown.cpp:26-31` v23, `:29-36` v24 |
| F7 | minor | first hit, unspent | **CLOSED** | — |
| F8 | minor | D7 extended to point reads (DEC-78) | **CLOSED** | Wording nit n7: Core's `Read` also ignores trailing bytes |
| F9 | major | parking, compression, a model check | **CLOSED** for my schedule; see R2-M6 | The rev1 model reproduces my F9 schedule under X17 and passes the grid. A related schedule outside the model's abstraction deadlocks (R2-M6), and Lemma 2 has a second hole (R2-m7) |
| F10 | major | `archive`, `DurableBelow`, the journal | **CLOSED** for archives | `DurableBelow` is sound (§3.3 below). The same section's state rule, though, moves blockfilterindex's file position backward (R2-M2) |
| F11 | major | b1 = flush + recovery, subscriber-free; b2 = prune; b3 = subscribers | **CLOSED** | Every T1–T41 is assigned once (checked); ST3a's owner confirms the `None` variants |
| F12 | major | ancestry resolution; rev files rewritten into rdashd-owned inodes; stale blocks cleared | **CLOSED** | temp → sync → rename over the link → dir sync → index batch is the right order. Positions shift, because a normalised entry gains its code bytes and the 0x00, and rev1 updates every position in the file. Ancestry resolution handles GPT's A100/B110 case |
| F13 | minor | `dirty_order` dropped | **CLOSED** | See R2-M5 for a different fallback problem |
| F14 | minor | synchronous manual prune; deferral keeps the check armed | **CLOSED** | — |
| F15 | minor | `deregister`, two-flush shutdown, "no DIRTY" shortcut | **CLOSED** | See R2-m7 for a shortcut interaction |
| F16 | minor | PMU counters plus a sign test (DEC-80 Q2) | **CLOSED** | — |
| F17 | minor | `NonNull` from `Box::into_raw` | **CLOSED** | Borrows derive from raw pointers to separate allocations, and the `Vec` moves only `NonNull`s. Sound under Stacked and Tree Borrows |
| N1–N8 | nit | all applied | **CLOSED** | N2 re-checked: `m_synced` is assigned at `index/base.cpp:78` v23 and `:90` v24 |

---

## 2. The two declared deviations

### D12: cache membership across a hidden freeze (GPT m1)

**Judgement: accept the deviation, but its rationale is wrong, and an FT depends on it.**

D12 (`:1160`) and §2.4.1 (`:362`) say membership changes only "which coins ATMP's `coins_to_uncache` frees … never a verdict". But Core also uses `HaveCoinInCache` to **choose a verdict**:

```cpp
// validation.cpp:851-860 v23 / :970-979 v24
if (!m_view.HaveCoin(txin.prevout)) {
    for (size_t out = 0; out < tx.vout.size(); out++) {
        if (coins_cache.HaveCoinInCache(COutPoint(hash, out)))   // "Optimistically just do efficient check of cache"
            return state.Invalid(TX_CONFLICT, "txn-already-known");
    }
    return state.Invalid(TX_MISSING_INPUTS, "bad-txns-inputs-missingorspent");
}
```

The two results differ visibly:
- the `testmempoolaccept` reject reason;
- P2P handling: `TX_MISSING_INPUTS` sends the transaction to the orphanage and requests its parents, while `TX_CONFLICT` goes to recent-rejects.

`mempool_accept.py` asserts `'reject-reason': 'txn-already-known'` immediately after `generate(node, 1)` (`:81` v23, `:107` v24). Core's answer depends on whether it has flushed since that block. Ours depends on whether a freeze **and its release** happened in between. Our own freeze causes (LEFT_IBD, DMAX, the f·B threshold) are events Core does not have. So one LEFT_IBD freeze on the FT's first fresh block, released within milliseconds on regtest, turns the reply into `missing-inputs`.

**Required:**
1. Restate D12 as **verdict-affecting in the D1 class**: Core's own reply depends on its flush timing, and ours on freeze/release timing.
2. Add an MP1 regression: mine, force an own-cause freeze and release, then call `testmempoolaccept`.
3. If that flips the FT, MP1 needs L8-style retention of F's unspent entries as members after release, or a probe that survives our own freeze causes.

The `uncache` half is memory-only, as stated.

### D13: v24 commits the index locator at `pprev` on every disconnect; rev1 keeps that in memory only (my F2's durable half)

**Judgement: accept, with two wording fixes.**

- **What actually differs.** rev1 already makes a Durable rewind commit for disconnects **at or below** L (§2.9 item 5). D13 is only about disconnects of blocks above L. Core v24 commits at `pprev` there (`index/base.cpp:238-259`). We cannot when `pprev` is a strict descendant of G, because of R11a.
- **"RPC answers are identical" is too strong.** After a crash, Core v24 restarts at `pprev`, and if that is the tip it is synced at once (`index/base.cpp:90`). rdashd restarts at the lower L, runs `ThreadSync`, and reports `synced: false` at a lower `best_block_height` until it catches up, as v23 would. Say "identical once catch-up completes".
- **"Would cost … the blind rewind" (`:75-77`) is inaccurate.** A capped Durable commit at `LCA(pprev, T_ack)`, made whenever that is above L, is legal under R11a. It would match v24 whenever `pprev ⪯ G`. The real costs are one fsync per disconnect and the remaining case where `pprev` is ahead of G. Record that alternative and leave the choice to the manager.

---

## 3. The new mechanisms: what holds

### 3.1 The rewind gate (§4.2 step 3b, §4.4.1 rule 4)

- **Condition.** "Some durable L strictly descends from `new`" is exactly the negation of ST3a's R11a: "not ahead" means not a strict descendant of G (`ST3a-power-loss.md:634`). The plain "new below the old tip" test would miss a stale-branch L, as rev1 notes.
- **Commit cap lowered to `new` while the gate is open.** This stops a Syncing subscriber from committing at the old `T_ack` during the gate. It is needed, and it is modelled (`cap_of`).
- **Targets.**
  - A parked Synced target woken by the request drops its head `Connected(k)`, because k descends from L and so is not an ancestor of `new`. It ignores the matching disconnect and processes the disconnects below. They never meet the limit.
  - A Syncing target rewinds at once. Core's own `ThreadSync`, with `pindex` on a stale branch and the tip at the fork, would commit the stale locator and mark itself synced (`NextSyncBlock` returns null: `index/base.cpp:164-170` v24). The design's immediate rewind is the right departure.
- **The rewind commit itself.** Core's `CustomRewind` copies heights `[new_tip, current_tip]` (`blockfilterindex.cpp:349`, `coinstatsindex.cpp:272` v24). Ours archives `(F, p]`; the fork row stays on the chain, so it is equivalent. Core never deletes stale height rows, and `LookupOne` checks the hash (`blockfilterindex.cpp:362-378`). Ours deletes them and serves the archive, which gives the same answers.
- **Model.** X19 is caught (re-run).
- **One gap,** in the claim that "the disconnects down to `new` are in its queue, because V emitted them before freezing". Rule 5 lets V freeze inside its backpressure wait, before the event for the tip change it just made has been emitted. For the gate, my stepwise model variant (R2-M6) still catches X19 and found no gate-specific failure. The same hole in case A/B is a real deadlock.

### 3.2 Parking and the skip rules (§4.4.1 rules 1–3)

- I re-ran `liveness_model.py --quick`. All 21 directed and grid checks pass, and X17–X20 are each caught (`/work/scratch/ch2-r2-opus/model/`).
- Case B's drop rule is sound. A queued `Connected(b)` before `Connected(new)` with b not an ancestor of `new` must have its `Disconnected(b)` between them, so the "ignore the matching disconnect" rule is total.
- Compression of the head pair is legal under ST3a's net-path drain.
- Two holes remain in the argument, not in the rules' intent:
  - R2-M6: the case-A test is defined by queue membership;
  - R2-m7: the freeze condition says "active has changes".

### 3.3 Per-block state journals and `DurableBelow`

- **Barrier coverage holds.** A commit at X writes only after a submit of everything through `p ⪰ X`, then `sync_files`, then `sync_wal`. After it, rows at heights ≤ h(X) change only after a rewind commit lowers L. Every write in between is to heights above L, hash-keyed archives, or state keys in later locator batches. The rewind commit runs before the next connect, so its reads at ≤ h(L) see only barrier-covered rows. Item 13 holds.
- **The `reverse` reads match Core's `ReverseBlock`.** Core reads the height row at h − 1 (`coinstatsindex.cpp:417` v24); for h ∈ (F, L], h − 1 ≤ h(L). Core's hash-row fallback (`:421-431`) is never needed below L, because those rows are on L's chain.
- **The state contract fixes GPT M3.** `DB_MUHASH` goes only in locator batches (`coinstatsindex.cpp:231-233, 394-399`), and the state at any X in `[L, p]` comes from the journal.
- **Gaps:**
  - the visibility of staged rows (R2-M1);
  - the resource high-water mark on backward state moves (R2-M2);
  - non-height-keyed stores (R2-M3);
  - journal memory (R2-m10);
  - reads at open (n3) and per-height scans (n4).

### 3.4 Rule O (§2.5)

- B(t) equals `nTotalSpace`, and the CRITICAL equivalence holds. Freezes happen only at op boundaries, and ATMP and RPC ops are boundaries on V.
- The oversized path (step 4) is reached only from an empty cache. The release ordering (`Arc` dropped before `FlushDone`, `strong_count == 1`) closes GPT's race.
- The gap is the growth term (R2-m1). The fallback half of it is large (R2-M5).

### 3.5 `GuardedEngine::corrupt` (DEC-78)

- It matches DEC-78 and DEC-80 Q8: trip first, then `read_failed` with Core's read-error pair, then abort. `CoinsReadError` stays constructible only after a trip.
- CS5a's trait matches §2.4.4 (`view.rs:34-56`, `type Error: fmt::Debug`, `Infallible` for memory views).
- The gap is coverage of snapshots and the cursor (R2-m9).

### 3.6 FA7

- Clause 1 is the useful one. Under X16a, `fetch` returns `Ok(None)`, so the connect returns a validation failure rather than `Fatal`. Under X16b there is no trip. Under X21 a corrupt coin is read as missing.
- Clause 2's window is empty under the seam (R2-m3).

---

## 4. The CH2a/b/c phase plan

- **CH2a.**
  - a1: the gate needs R2-M5's capacity metric. Otherwise, at 6.4 M coins the arena misses the 20 % bar while being far better at the DEC-43 point.
  - a2: rule O's cases need an op that crosses a resize (R2-m1).
  - a3: `corrupt_coin_aborts` is right. Add the snapshot and cursor paths (R2-m9).
- **CH2b.**
  - b1 and b2 are consistent. T1–T41 are partitioned b1/b2/b3/CH3a with none missing or duplicated.
  - b3 carries every new major. Its model port must include stepwise emission (R2-M6).
  - C8(c) must read archived filters back through their positions after a reorg **above** L followed by new connects (R2-M2).
  - C8 also needs an IX2-shaped keyed subscriber (R2-M3).
- **CH2c.**
  - Fix the PLAN row's memory criterion (R2-m2).
  - The proxy must include a txindex-shaped subscriber and count DMAX flushes (R2-M4).

---

## 5. New findings

### R2-M1 (major, present since rev0): staged subscriber rows are invisible to queries, while `best_block()` already reports them

**Where.**
- `staged` is "rows of journal entries not yet submitted" (`:781`).
- Item 3.1 submits them only inside a commit (`:811`).
- Queries consult `pending_archive` "before the DB" (`:797`) and nothing else.
- A Synced subscriber commits only on a `FlushCommit`, a checkpoint or a rewind.

**Core.** `CustomAppend` writes each block's rows to the DB before `SetBestBlockIndex` (`index/base.cpp:294-300` v24, `:277-282` v23). An RPC calls `BlockUntilSyncedToCurrentChain()` (`rpc/blockchain.cpp:1265, 3055` v24) and then reads.

**Scenario.**
1. Mine 4 blocks, as `rpc_getblockfilter.py:24-35` does, with no flush.
2. Call `getblockfilter(<tip hash>)`.
3. Core returns the filter.
4. rdashd's `best_block()` is the tip, but its height row sits in `staged`, so it returns "Filter not found". The same holds for `getrawtransaction` with txindex and `gettxoutsetinfo hash_or_height=<tip>` with coinstatsindex.

Rows stay invisible for up to a whole flush interval, which is up to `D_max` blocks. `:805` says "Height rows are written when staged", which reads either way. The ST3a toy batches deliberately (S11d, "staged data").

**Fix**, one of:
- **(a) Submit each block's rows Buffered as it is processed,** as Core does. ST3a §5.6 item 6 allows per-block data. The journal keeps only what archive and state need, and `s = p` always.
- **(b) A read-through view** `journal → pending_archive → DB` that every IX1/IX2 lookup must use, stated as part of the contract.

Option (a) is simpler, but it interacts with R2-M3. State the choice, and add an FT-shaped C8 case: connect, then query at the tip with no flush.

### R2-M2 (major, new in rev1): the resource high-water mark moves backward on a disconnect or a rewind, so stale filters' bytes get overwritten

**Where.**
- Item 4: "The new state is the previous journal entry's, or `durable_state`" (`:816`).
- Item 5.4 writes `commit_state(state_at(F), latest)`. By then the disconnects above L have already popped the journal, so `latest` is `durable_state`.
- Only `commit_state` (`:761`) knows about high-water marks, and only for what it writes.

**Core.**
- `m_next_filter_pos` only ever advances (`blockfilterindex.cpp:276, 289` v23; `:296, 309` v24).
- `CustomRewind` writes the *current* position (`:336 / :356`).
- `WriteFilterToDisk` writes at the given position, so going back means overwriting.

**Scenario.**
1. L = 100, p = 105. A one-block reorg disconnects 105; its archive goes to `pending_archive`, pointing at offset P₁₀₅.
2. The state reverts to 104's, whose next position is P₁₀₅.
3. `connect(105')` writes 105′'s filter at P₁₀₅.
4. `getblockfilter(<105 hash>)` now reads 105′'s bytes. The checksum fails and the RPC returns "Filter not found". Core returns 105's filter.

This is exactly `rpc_getblockfilter.py:43-47`, the stale-chain loop. The rewind batch also writes `DB_FILTER_POS` below bytes that durable archive rows reference, which contradicts item 7 (`:831`).

**Fix.** Make the high-water fields driver-owned and monotone for the whole process: `latest` is the maximum over everything processed so far, not the last journal state after pops. Apply `commit_state(prev, latest)` to the **in-memory** state whenever the driver moves state backward: a disconnect above L, a rewind commit, or the gate. Extend C8(c): after a reorg above L and new connects, read each archived filter back through its position.

### R2-M3 (major, new in rev1): the §2.9 row classes cannot express IX2's stores; nothing removes their rows on a disconnect, a rewind or at recovery

**Where.**
- `RowSchema` (`:733-738`) has four classes: height-prefixed, hash archive, fixed state keys, and inexact ("never deleted").
- rev1 removed rev0's `stage_disconnected` (rev0 `:610`).
- Item 4 only drops unsubmitted staged rows.
- The rewind and recovery do blind `height_ranges` deletes only.
- §1.3 and §3.5.1 make IX2's address, spent and timestamp indexes `SubscriberData` implementations under both profiles.

**Core v24.**
- `AddressIndex` keys by address type, hash and height. Its unspent index is a **mutable keyed set**.
- `CustomRewind` reads each block plus its undo, newest first, and its `RewindBatch` both erases history rows and **restores** unspent entries (`index/addressindex.cpp:144-161, 280-395`).
- `SpentIndex` (keyed by outpoint) and `TimestampIndex` (keyed by timestamp ‖ hash) erase in the same way.
- None of these keys starts with a height. The unspent index cannot be "inexact".

**Scenario.**
1. `feature_addressindex.py:217-223` mines a block that spends an address's UTXO, then calls `invalidateblock`.
2. In rev1, if the block's rows were submitted, its history row stays and the unspent entry it erased is never restored.
3. `getaddressutxos` misses an unspent output, and `getaddresstxids` lists a transaction that is not on the chain, until the next restart and beyond.

`feature_spentindex.py:142-163` and `feature_timestampindex.py:54-66` exercise the same path. After a crash, keyed rows above L cannot be range-deleted blindly either.

**Fix.** Add a **keyed class** with `disconnect_rows(block, undo) -> rows`, computed from block and undo files only. That is blind with respect to the DB, as Core's `RewindBatch` is. Then choose one of:
- **(a) Locator-only keyed rows.** They are written only in locator batches. A disconnect above L pops them from the journal. A rewind applies `disconnect_rows` for `(F, L]` in the rewind batch. Recovery has nothing to undo. Queries read through the journal (R2-M1 (b)).
- **(b) Per-block Buffered rows with a durable undo window.** Each submit batch also writes the block's inverse rows under a window key. A disconnect above L applies and removes them. Recovery reads the window once, as a decision key, and applies the inverses blindly. This does not depend on undo files above G, which may not survive.

Alternatively, carve IX2 out of §2.9 explicitly and name its mechanism. Add an IX2-shaped keyed subscriber to C8, with a reorg above and below L and a crash at every event.

### R2-M4 (major, present since rev0): `D_max = 1,024` blocks makes DMAX the dominant flush cause whenever a mandatory subscriber exists, which the DEC-43 cell has

**Where.** `d_max: u32 /* 1,024 */` (`:784`); rule 3; the DMAX cause in §3.3. DEC-69 accepted "forced flush at `D_max`", but no decision fixes the value.

**Mechanism.**
1. A Synced subscriber's L moves only at commits, which follow each B1.
2. At `p = L + D_max` it parks and posts `FlushDue(DMax)`.
3. So with txindex on, which is node-bench's DEC-43 configuration (§2.5's 252.75 MiB), V must make a **full flush every ≤ 1,024 blocks**: B1 Durable, the sub-batches, the pre-Bk `sync_wal`, Bk Durable, two subscriber fsyncs, and `flush_memtables` (0.16–0.82 s in ST1 D2).
4. In the M3 cell's pre-DIP3 `reindex-chainstate` window, Core replays 1,024 early blocks in a fraction of a second. That gives about 1,000 forced flushes, and with one F at a time, V is throttled to W's per-flush latency.
5. The FRESH window shrinks to ≤ 1,024 blocks, so many more coins hit the DB as a put followed by a delete.

The CH2c proxy (`coins-replay-*-dec43`) has no subscriber, so it cannot see any of this. The first measurement would be the M3 gate.

**Fix** (DEC-69's rule unchanged):
- Make `D_max` **per subscriber**, sized by journal bytes rather than blocks.
- Set `D_max = ∞` for a subscriber with only inexact rows (txindex). DEC-77 Q6 already puts it outside R11c's exact scope, and it has no state and no archive.
- A blind `delete_range(h(L), ∞)` is exactly as blind as `(h(L), h(L) + D_max]`. A finite `D_max` is needed only to bound the journal and the ST3a model.
- Add a txindex-shaped subscriber to the DEC-43 proxy, and report forced-flush counts in `coins-replay-*`.

### R2-M5 (major, present since rev0; matters if the fallback ships): 64 uniformly filled shards double nearly together, so the fallback has the unsharded table's cliff, and the layout gate's sample points can choose it wrongly

**Where.**
- §2.3 `:305-313`: "a shard doubles independently … holds as many coins as Core at equal B, which is enough for the DEC-43 cell on density alone".
- §5.2 `:1455`: fallback "≈ 2.15 M (parity)".
- The gate measures bytes per coin at 1 M and 6.4 M coins (`:315`).

**Why.** With uniform hashing, every shard holds `n/64 ± O(√(n/64))` entries, so all 64 reach 7/8 load within about 1 % of each other. A layer can only grow past that point if *every* shard doubles, and the shards that cannot double block further growth. `shard_cliff.py`, with hashbrown's 7/8 load and 73 B per bucket, gives these results at the DEC-43 `f·B` = 132.5 MB:

| Layout | Entries when growth stops | Memory used | Bytes per entry |
|---|---|---|---|
| unsharded | 917,504 | 76.5 MB | 83.4 |
| **64 equal shards** | **921,958** | 131.6 MB | **142.7** |
| 64 shards weighted ∝ 2^(i/64) | 1,098,228 | 131.6 MB | 119.8 |

So the fallback holds active + F ≈ **1.84 M** coins against Core's 2.20 M, which is −16 % and the same as the unsharded map that §2.3 rejects.

**The gate's snapshot points disagree** (`gate_points.txt`):

| Coins | Arena | Fallback | Arena below fallback |
|---|---|---|---|
| 1 M | 82.6 B/coin | 153.1 B/coin | 46.1 % |
| 6.4 M | 78.6 B/coin | 95.7 B/coin | **17.9 %** (bar: 20 %) |

At 6.4 M the arena *fails* DEC-77 Q5's 20 % bar, and the fallback would ship, even though at the operating point that matters the fallback is the one at the cliff.

**Fix** (threshold unchanged; measurement only, like DEC-80 Q2):
- Gate on **"coins held when the freeze fires"** under rule O's accounting, at the DEC-43 B and over a sweep of B (or the mean over a log-uniform sweep of n), not at two fixed counts.
- Desynchronise the fallback: shard weights ∝ 2^(i/64), or staggered proactive `reserve`.
- Correct §5.2's fallback row.

### R2-M6 (major, new in rev1, found with a model variant): V may freeze before `Connected(new)` has been emitted; the case-A test then acks at `p ≺ new`, and the subscriber later parks with nothing left to freeze

**Where.**
- Rule 5 (`:1307`) and §4.2 (`:1226-1230`): every V wait, **including the backpressure wait while emitting an event**, services `FlushDue` by freezing.
- Rule 1, case A (`:1284`): "`Connected(new)` is not ahead in the queue. Either it was processed (`last_seq ≥ connected_seq`) or it was never delivered."
- Lemma 2 (`:1341-1344`).
- The rev1 model connects only when every queue has room, emits atomically, and never freezes between a tip change and its event (`v_moves`, `liveness_model.py:89-120`). It cannot reach this state.

**Variant.** `model/stepwise_reorg.py` makes V change its tip, then emit the event when every queue has room. A freeze is allowed while the emission is pending, which is what rule 5 permits. Disconnects are emitted one at a time. Everything else is rev1's model, unchanged.

**Witness** (smallest configuration: SS, shared thread, `D_max` = 1, Q = 1, no reorg, MAXH = 3):

```
V.conn(1) V.freeze@(1)(pending) W.B1(1) S0.commitA@0 S1.commitA@0 V.emit(C1) V.conn(2) W.Bk V.release
V.freeze@(2)(pending) W.B1(2) …  →  terminal: V tip 3 = last frozen tip, F none, T_ack 3;
both subscribers p = 2, L = 1, parked at Connected(3) (limit 2), FlushDue posted, no outstanding commit
```

**What happens.**
1. At each B1, `Connected(new)` is still in V's emission wait, so it is "not ahead".
2. Case A commits at `LCA(p, new) = p` and acks.
3. Later the subscriber reaches its limit at a block V connected **before** the last freeze.
4. Lemma 2's "captured ⇒ drained to `new_last` ⪰ b" fails here, because the capture was answered by case A.
5. V's tip equals the last frozen tip, so V has nothing to freeze, and nothing wakes the subscriber.

With the queued copy read as an always-skippable no-op (`stepwise_skipF.py`), 25 of the configurations it completed still fail.

**Fix.**
- Decide case A/B **by sequence number**: case B iff `connected_seq > last_seq` and `connected_seq ≥ seq_synced`, the first sequence number the subscriber receives after turning Synced. That treats a pending emission as "ahead" and keeps "never delivered" as case A. The text's parenthetical already points this way; the queue-membership sentence and the model contradict it.
- For this to work, V must assign an event's `seq` at the tip change, before the emission wait. The packet must then carry `connected_seq` (n2), and a subscriber must record `seq_synced` as the next `seq` when it turns Synced.
- Restate Lemma 2.
- Say explicitly that the queued `FlushCommit` copy is a no-op whenever it is dequeued, before or after the ack.
- Port the stepwise emission into b3's Rust model, and add the witness as a directed regression.
- **Result with the sequence-number test** (`stepwise_seq.py`, rev1's rules otherwise unchanged):
  - every one of the 60 stepwise configurations passes: 3 class mixes × shared or per-subscriber thread × 5 (`D_max`, Q, window) × reorg depth 1–2, about 4.6 M states, in `model/seq-results.txt`;
  - the smallest depth-0 configuration passes;
  - X19 is still caught.

  So the fix is sufficient within this abstraction. Without it, every configuration I ran failed (`stepwise-results-partial.txt`).

### Minors

- **R2-m1 (F1's residual): `T(op)` omits the layer growth an op can trigger.**
  - `GROW` is "one growth step: an arena chunk of 262,144 B, or a shard's resize transient" (`:498`), and `Usage` has no growth term (`:465`). Yet A-1 (`:1835`) lists "growth headroom" in the invariant.
  - The arena's index doubles over **all** of active's entries. At the DEC-43 point that is 2^21 → 2^22 buckets: +21 MB permanently and +10.5 MB transiently, against a block's `T(op)` of a few MB.
  - In the fallback, a block near the synchronised point (R2-M5) triggers several shard doublings, at ≈ 2.4 MB each.
  - **Fix:** at the op's start, `reserve(2·(n_in + n_out))` on active and the child, so growth happens at the boundary where rule O can freeze or wait. Include `layer.growth_bound(n)` in `T(op)`. Add a2 cases that cross an index or shard resize.
- **R2-m2 (F4's residual):** align the CH2c PLAN row (`:1758`, "RssAnon+RssFile") and the W-MC rule (`:1568`, "RSS (anon + file)") with §5.5's `rss_anon_peak` gate. Keep RssFile and `memory.peak` as reported metrics.
- **R2-m3 (FA7 wording):** "between the fault and `Mark::Abort`" (`:1664`) is empty under the seam, because the sink records `Abort` when the latch trips (§2.4.4 rule 3). Say "from the fault until the epoch ends".
- **R2-m4:** restate D12 (§2 above).
- **R2-m5:** reword D13 (§2 above).
- **R2-m6: archives for blocks disconnected above L are lost on a crash.**
  - `pending_archive` lives in memory until the next locator batch. Recovery's blind `height_ranges` delete then removes the stale blocks' height rows, so `getblockfilter` or `gettxoutsetinfo` for such a block fails after a process kill.
  - Core's writes survive a process kill: v24 at the disconnect, v23 at the next connect, both unsynced but in the page cache.
  - **Fix:** declare it as D14, or after `sync_files()` write archive rows Buffered at the disconnect. They are hash-keyed, idempotent and a monotone set, so early is safe.
- **R2-m7: Lemma 2's freeze condition.**
  - Rule 5 freezes "if no F is in flight and active has changes" (`:1307`). Lemma 2 claims that b, or a later change, is in active (`:1344`).
  - A block whose coinbase pays only unspendable outputs adds no coin (quirk 33). On regtest, `generatetodescriptor raw(6a)` produces such blocks.
  - ≥ `D_max` of them plus a full queue leaves active empty, a subscriber parked and V in backpressure. That is a deadlock.
  - The model freezes on `since > 0`, which is tip-based, so it does not see this.
  - **Fix:** freeze iff V's tip ≠ the last frozen tip, or active has DIRTY entries. Never take §4.2's index-only ALWAYS shortcut, which issues no `FlushCommit`, while a `FlushDue` is pending.
- **R2-m8 (model fidelity, folded into R2-M6):** the rev1 model enqueues a reorg's disconnects atomically and past Q (`V.reorg`). Port the stepwise variant.
- **R2-m9: `GuardedEngine` coverage of snapshots and the cursor.**
  - `CoinsTable::{get, multi_get, cursor}` take `impl KvRead` and return `StoreError` (`:570-575`).
  - The cursor (the copy points) and CH4's `multi_get` read engine **snapshots**. Unless `GuardedEngine`'s snapshot and iterator types are guarded too (trip on error, `corrupt` reachable), a snapshot read error bypasses the latch, which reopens X16b's class.
  - **Fix:** `type Snapshot = GuardedSnapshot`. Type `CoinsTable` over guarded handles and have it return `CoinsReadError`. Make `corrupt` `pub(crate)`. Add the cursor and snapshot paths to `corrupt_coin_aborts`.
- **R2-m10: journal memory.**
  - "≈ 1.5 MiB at the default" (`:809`) counts state only, but `JournalEntry` holds every row (`:786`). For txindex that is about 1,024 × transactions-per-block rows, which sits outside B and inside RssAnon, the DEC-43 gate.
  - **Fix:** bound the journal in bytes, keep only the rows that archive or state need, and account for it in `engine-memory` or W-MC. This goes with R2-M1 (a) and R2-M4.

### Nits

- **n1.** Add the CH3a **acceptance** edit to §7.4: T12, C11's connect half, and the T20/T21 connect halves. `:1764` edits only deps and BG.
- **n2.** `FreezePacket` (`:606-615`) has no `connected_seq`, but step 5's `publish_durable(new, connected_seq)` needs it.
- **n3.** Item 6.1 (`:826`) reads only the locator and state keys at open. Core's coinstatsindex restores its totals from the **height row at L** (`CustomInit → LookUpOne`, `coinstatsindex.cpp:350-389` v24). Either allow a `DurableBelow` read at open or require State to sit wholly in state keys.
- **n4.** `DurableBelow` offers only point reads (`height_row(cf, height, key_rest)`, `:774`). Archiving `(F, L]` needs every row at a height, so add a per-height prefix scan.
- **n5.** The rewind commit is made "at the deepest fork the queued disconnects reach before the next connect". With a small queue the subscriber sees one disconnect at a time. Defer the commit to the next connect, a `RewindRequest` or a `FlushCommit`; that is safe, because G has not moved. Otherwise it is one Durable commit per disconnected block.
- **n6.**
  - Rule O step 3's freeze has no `FlushCauses` bit; name one.
  - Step 4's "empty cache" overshoot bound ignores prefetch (≤ B/16).
- **n7.** D7: Core's `Read` has no end-of-stream check (`dbwrapper.h:314-316 / :324-326`), so a value with trailing bytes reads as a **present** coin. Specify `PackedCoin::from_db`'s trailing-byte rule; CS5a's `CoinRef::decode` is a streaming decoder (`coin.rs:154`). Say D7 covers both cases.

---

## 6. Summary by severity

| Sev | # | Finding | Scope item |
|---|---|---|---|
| major | R2-M6 | V may freeze before `Connected(new)` is emitted; case A by queue membership then deadlocks (model witness) | 3 |
| major | R2-M1 | staged subscriber rows are invisible to RPC lookups until the next flush; `rpc_getblockfilter` at the tip fails | 3 |
| major | R2-M2 | the blockfilter position moves backward; stale filters are overwritten; `rpc_getblockfilter`'s stale loop fails | 3 |
| major | R2-M3 | IX2's keyed stores fit no row class; reorgs leave wrong `getaddressutxos` and `getaddresstxids` results; three IX2 FTs | 3 |
| major | R2-M4 | `D_max = 1,024` forces a flush every ≤ 1,024 blocks with txindex: the DEC-43 cell's main risk, invisible to the proxy | 4 |
| major | R2-M5 | the 64-shard fallback has the unsharded cliff (≈ 922k per layer); the layout gate's sample points can pick it | 3, 4 |
| minor | R2-m1 – m10 | `T(op)` growth; F4's text; FA7's window; D12 and D13 wording; crash-lost archives; the coin-free freeze; model fidelity; guarded snapshots; journal bytes | 1–4 |
| nit | n1 – n7 | CH3a row; `connected_seq`; reads at open; prefix scan; rewind trigger; `FlushCauses`; trailing bytes | — |

**Round-1 tally:** of my 17 findings, 14 are CLOSED (F3 with a wording fix, R2-m3) and 3 PARTIAL (F1, F4, F5). All 8 nits are CLOSED.

**Strengths.**
- rev1's response is thorough.
- Building an explicit-state model to back the deadlock argument was the right move. It found three holes that neither reviewer had, and it made R2-M6 cheap to find: a ten-line variant.
- The rewind gate is a real catch: ST3a's toy never exercised it.
- The F12 import mechanics are clean.
- The state contract is the right shape for coinstatsindex.

## Verdict

**APPROVE-WITH-CHANGES**, staged:
- **Before CH2a a1 merges:** R2-M5's gate metric and the fallback's desynchronisation, and R2-m1.
- **CH2b b1 and b2** may proceed on rev1, with R2-m3 and R2-m9.
- **Before CH2b b3 starts:** a rev2 of §2.9 and §4.4.1 that fixes R2-M1, M2, M3 and M6 and sets R2-M4's parameter. A reviewer's closure check of that rev2 must also include the stepwise model.
- **Before CH2c:** R2-m2, and R2-M4's proxy subscriber.
- **The minors and nits** can go into the same rev2.
