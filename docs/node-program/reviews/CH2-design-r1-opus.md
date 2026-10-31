# CH2 design review r1 (Opus): coins cache, coins table, flush/recovery/subscribers

| | |
|---|---|
| Design | `designs/CH2-coins-flush.md` (rev0 reconciled, fca00cb8 on `node/integration`) |
| Reviewer | Opus 5.5, independent (not a designer of CH2-fable, CH2-opus or the reconciliation) |
| Inputs checked | ST3a rev5 §2.4, §5.1–§5.6, §6.2; DEC-43, -59, -62, -63, -64, -66, -69, -74, -77; Core `src/` at v23.1.8 and v24.0.0-rc.3 (extracted under `/work/scratch/ch2-opus/{v23,v24}`); memory notes on Q10, WAL non-prefix recovery, macOS fullfsync, coins-cache memory |
| Status | Final (r1) |

## Verified so far (no finding)

- `FlushStateToDisk` is identical at both pins apart from `IsPruneMode()`, `GetParams()`, `EvoDbIdentity` and the active-chainstate guard on `ChainStateFlushed` (v23 `validation.cpp:2737-2911` vs v24 `:2945-3120`, diffed). Every trigger in §3.3's table matches (`:2828-2838 / :3036-3046`), as do `fFlushForPrune` `:2812 / :3020`, the check clear `:2809 / :3017`, the disk-space aborts `:2843, 2881 / :3051, 3089`, "System error while flushing" `:2908 / :3117`, `ChainStateFlushed` `:2905 / :3114`, `PRUNE_LOCK_BUFFER` `:115 / :122`, the intervals `:97, 99 / :104, 106`, and the call sites `:3070, 3207, 3550, 4471 / :3304, 3457, 3851, 4855`.
- `CCoinsViewDB::BatchWrite` is byte-identical between pins (`txdb.cpp:104-165 / :113-174`). The marker-in-first-batch, the partial-batch condition `SizeEstimate() > batch_size`, the crash roll after each partial batch only, and `count` counting every entry are as §3.4/§3.6 say.
- `ReplayBlocks` strings and lines (`:4888-4943 / :5232-5294`) match §3.5.1. The EvoDB check is against `pindexOld` when DIP3 is active at `old` (v23 `:4909-4912`).
- `CCoinsViewCache` semantics (`coins.cpp:44-297`, v23 read in full). §3.2's commit table matches `BatchWrite` `:182-252`; `AddCoin`'s FRESH rule `:70-108`; `Uncache` clean-only `:284-297`.
- Legacy undo: `TxInUndoFormatter` identical at both pins (`undo.h:22-49`); the 0x00 dummy for height > 0 is right. Dash's per-txout merge `83ec12f1a0` (PR #1701, 2017-11-01) is in `v0.12.2.2` and not in `v0.12.2.1` (`git merge-base --is-ancestor`). Fable's "always carried the height" is wrong, as §8 row 15 says.
- `CalculateCacheSizes` (v24 `node/caches.cpp:15-42`) and the EvoDB fixed `64 << 20` (`evo/evodb.cpp:35 / :38`) match §2.5. LevelDB's per-DB split n/2 + n/4 (`dbwrapper.cpp:113-114` v23) matches.
- The DEC-43 arithmetic at `-dbcache=300` with txindex on (coins 252.75 MiB) matches the memory note.

## Findings

(numbered as found; sorted by severity in the final section)

### F1 (major): the budget invariant cannot hold mid-block when no F is in flight, and the design gives no overshoot rule

**Where:** §2.5 "The invariant, checked before every growth of active, the overlay or prefetch, and asserted after every operation in tests"; §6.7 "after every op in `coins_cache_model` … `usage().total() + growth_headroom ≤ B(t)`"; §7.1 a2 acceptance "budget assertions (§6.7) for every f".

**Scenario.** A freeze can happen only at a block boundary. Mid-block, with F absent, nothing can free memory: V cannot freeze and has nothing to wait for. `feature_dbcrash` node0 runs `-dbcache=4 -maxmempool=0` (`feature_dbcrash.py:53-61` v24). So `B = coins = 1.75 MiB`: total 4 MiB, block tree 0.5, coins DB `min(1.75, 0.875 + 8)` = 1.75 (`node/caches.cpp`). `generate_small_transactions` puts up to 2,500 two-in, three-out transactions in a block (`:186-203, 239`): about 7,500 adds and 5,000 spends. The overlay holds about 12.5k slots (≈ 1 MB at 80 B), and `commit` copies them into active while the overlay still holds them. That peaks at ≈ 2 MB > B before any boundary is reached. Core goes over too, by design: CRITICAL is evaluated only at `IF_NEEDED` after the connect (`:3207 / :3457`). Core's budget also counts only `CoinsTip()` (`:2743 / :2951`), not the child view, and ours counts the overlay.

Read literally, the design does one of three things here. "At the limit V waits" hangs. The test assertion fails. Or the behaviour is undefined. §2.5's last bullet ("if B(t) shrinks … V freezes at the next block boundary") hints at overshoot but covers only mempool growth.

**Fix.** State the rule. With F in flight, V may wait mid-block for the release; the wait is a service loop that runs `release` through the overlay's `&mut` borrow. With F absent, growth proceeds past B, CRITICAL is raised, and V freezes at the next boundary, as Core does. Restate the asserted invariant: `≤ B(t)` at block boundaries, and mid-block `≤ max(B(t), usage_at_block_start + block_overshoot)`. Add a `coins_cache_model` case with B smaller than one block.

### F2 (major): `SubscriberDriver` cannot express O17 MPIX-11, and a literal port of v24's eager rewind breaks R11a

**Where:** §2.9 (`commit_locator` and `rewind_commit` are private; "every locator rule lives in the driver"), §4.4 "The rewind commit", §3.8 ("No new O17 row is needed"), §2.11's IX1 row.

**Core.** v24 adds `BaseIndex::BlockDisconnected` (`index/base.cpp:308-330` v24, Dash commit 0695e976b1, "move BlockDisconnected handling to BaseIndex"). On every disconnect of a block on the indexed chain it calls `Rewind(best, pindex->pprev)`, which runs `CustomRewind`, sets `m_best_block_index = pprev` and `Commit()`s the locator (`:238-259`). v23 has no `BlockDisconnected` in `BaseIndex`: the index rewinds lazily at the next `BlockConnected` (`:270` v23), and its `Rewind` has no `CustomRewind` (`:224-242` v23). O17 already models this as **MPIX-11** (`IndexRules.rewind_on_disconnect`, profile, RPC-observable; `O17-pin-delta.md:339`), and the PLAN IX1 row carries it.

**Problem.**
1. The design makes the driver the only place locator and rewind rules live. It provides only "a rewind commit iff the fork is below h(L)", so IX1 has no way to implement the v24 profile without changing CH2's API.
2. A literal port is unsafe. v24 commits the locator at `pprev` of the disconnected block. While V is ahead of the durable chainstate, `pprev` can be a strict descendant of G (the durable marker's `new`). A Durable locator there violates R11a / `DurableTipAtLeast`, which is T10's shape: a locator ahead of the chainstate.

**Fix.**
- Separate the subscriber's **in-memory best block** from its durable locator. The best block is what `getindexinfo` and the RPCs read. Under `rewind_on_disconnect` it moves to `pprev` on every on-chain disconnect.
- Keep the durable rule as designed: a Durable rewind commit only when the fork is below `h(L)`, blind deletes for the rest.
- Expose `IndexRules.rewind_on_disconnect` to `SubscriberDriver`. Add MPIX-11 to §3.8, and a C8 case under each profile: invalidateblock, query, crash, recover.

### F3 (major): X16 cannot be detected as specified

**Where:** §6.4 X16 ("a `CoinsReadError` mapped to 'missing coin' … caught by C7's `fetch` read fault: a block marked `FAILED_VALID` (R6(d) inverse)"); §2.4.4 rule 2.

**Why.** A `CoinsReadError` can be constructed only after `GuardedEngine` has tripped the node-wide latch. So if the plant maps it to `Ok(None)` above `GuardedEngine`, as the bug's text implies, every later write in the epoch is refused. That includes the `FAILED_VALID` status write. The bad verdict therefore never becomes durable, and no reopen-time check (R6(d), R6(e)) can see it. R8 passes as well, because the epoch still ends in `Mark::Abort`. The verdict is still harmful in production, before the abort: it can ban a peer, send a reject, or answer an RPC.

If the plant is instead inside `GuardedEngine`, swallowing the error without tripping, it is a different bug. R6(e) would catch that one.

**Fix.** Name the plant site. Add a **forward assertion**, in the style of FA1–FA6: after a read fault, the operation returns `Fatal`, and no `Connected`/invalid mark, block-status change, `ChainEvent` or mempool verdict is emitted. Catch X16 with it. Plant the `GuardedEngine`-swallow variant as well and catch it with R6(e).

### F4 (major): the DEC-43 proxy's memory criterion is biased, and its memory regime is unspecified

**Where:** §5.5 proxy ("RssAnon + RssFile ≤"); §7.4 CH2c BG.

**Bias.** The Core-model LevelDB arm mmaps its tables, so its SST pages count in RssFile. rdashd reads with buffered `pread` (§2.5), so its SST pages sit in the page cache and count in neither. The criterion therefore favours RocksDB structurally, while the risk DEC-43 names (OI-1: double caching under memory pressure) goes unmeasured.

**Regime.** ST1 already learned this and ran every cell inside a cgroup `MemoryMax` that holds anon memory and page cache together, gating on `rss_anon_peak` only (`ST1-bakeoff.md:20, 147-150`). The design never says that the proxy runs under that cap. Without one, a page-cache-leaning E wins on time by reading from free memory.

**Fix.** Run the proxy and W-MC under ST1's cgroup cap, with page cache inside it. Gate time on that. Gate memory on RssAnon, or on cgroup `memory.peak`, not on RssAnon + RssFile. State the same for the M3 exit cell, so its node-bench configuration matches.

### F5 (minor): some planted bugs and C11 exercise test stubs, not CH2 production code

T12 (`HAVE_UNDO` set before the undo write returns) and C11 (an accepted block that a stub connect marks invalid) concern undo writing and status bits. Those belong to CH3a and CH1, and in `ChainSut` they come from the "mini coins connect". Re-planting them in CH2b tests the stub. §6.4 says "re-planted against the production code", and the CH2b row lists T12.

**Fix.** List T12 and C11 (with T20 and T21's connect-side half) as CH3a acceptance, rerun with `ChainSut`'s connect replaced by the real one. Keep the CH2-side half of T20 and T21, a freeze or marker at an unapplied tip, in CH2b.

### F6 (minor): §3.6 misstates what the user sees on a flush error

**Where:** §3.6 row 4 and §8 row 21 ("Core's user-visible text is 'System error while flushing'").

**Core.** `AbortNode(state, msg)` with no user message (`shutdown.cpp:24-34` v23, `:27-38` v24) does three things:
- logs `*** System error while flushing: Fatal LevelDB error: …`;
- shows the **generic** "A fatal internal error occurred, see debug.log for details" (v23 `AbortError`, v24 `InitError` plus `EXIT_FAILURE`);
- calls `SetMiscWarning(msg)`, so the text appears in `getblockchaininfo` / `getnetworkinfo` `warnings` until shutdown.

The conclusion, that the three "Failed to …" strings are unreachable, is right (`CDBWrapper::WriteBatch` returns `true` or throws through `HandleError`, `dbwrapper.cpp:199-214, 260-267`; `CEvoDB::CommitRootTransaction` returns `WriteBatch`'s result, `evodb.cpp:56-64 / :98-108`).

**Fix.** Specify the `*** ` log prefix, the generic user message, and the misc warning. Extend D10's visibility note: the engine's inner text reaches RPC through `warnings`.

### F7 (minor): `have_in_cache` must mean "the first layer hit is unspent"

Core's `HaveCoinInCache` is `it != end && !it->second.coin.IsSpent()` (`coins.cpp:166-169`). §2.4.1 and §3.1 say "active ∪ F". Read as a union, a tombstone in active over an unspent F entry returns true. ATMP's `coins_to_uncache` bookkeeping then skips an uncache Core would do. That costs memory only, but it is a stated Core-equivalence.

**Fix.** "Resolve active, then F; true iff the first hit exists and is unspent."

### F8 (minor): D7 covers the cursor only, but point reads diverge too

`CDBWrapper::Read` returns `false`, which means no coin, when a stored value fails to deserialize (`dbwrapper.h:304-318` v23). §2.2's `PackedCoin::from_db` returns `Corruption` and aborts instead. CS5a's amended trait already documents this deviation (`view.rs` doc on `node/cs5a-coins` 17014d4b). It is reachable only on a corrupt DB, but it is a different outcome.

**Fix.** Extend D7 to point reads.

### F9 (major): §4.4's "one shared events thread is also safe" is false: there is a cycle through an in-flight F's step 6

**Where:** §4.4, the paragraph "One shared events thread is also safe" and edge-breaker 3; the b4 threaded test.

**Schedule.**
1. Flush F is in flight. At step 5 it captured synced subscriber S2, so W waits at step 6 for S2's ack.
2. Subscriber S1 was Syncing at F's step 5, so it is not captured. It finishes `ThreadSync` after F's B1 and makes its capped commit at `LCA(progress, T_ack) = new_F`, so `L = new_F`.
3. S1 now processes `Connected` events on the **shared** events thread. V keeps connecting while F is written (cheap early-chain IBD blocks fit in active easily), so S1 reaches `h(L) + D_max`.
4. S1 has no outstanding `FlushCommit`, so §4.4 has it "post `FlushDue(DMax)` to V and wait for a `FlushCommit`". That wait blocks the shared thread.
5. V cannot freeze, because F is in flight; `FlushDue` stays pending until release.
6. Release needs S2's ack. S2's events are queued behind S1 on the blocked thread.

That is a cycle. The queue then fills, and V sits in its backpressure wait for good.

The argument's flaw is the sentence "its unblocking needs only V's freeze and W's B1". While F is in flight, V's freeze needs F's release, and F's release needs **other** subscribers' acks. The 10⁴-schedule test uses per-subscriber threads, so it would not see this.

**Fix**, one of:
- (a) A subscriber at its limit with no outstanding commit **parks**: it stops consuming its own stream, and the dispatcher keeps serving the other subscribers. It never blocks a shared thread.
- (b) Make one thread per mandatory subscriber a CH1 requirement, and delete the shared-thread claim.

Either way, add the schedule above to the b4 test, using the shared-thread configuration and a subscriber that turns Synced after capture. Plant the blocking variant as X17.

### F10 (major): `SubscriberData::rewind_ranges` (blind deletes only) cannot reproduce Core's stale-block retention; two FTs fail

**Where:** §2.9 `rewind_ranges` ("key ranges only, never a read"), §4.4 "The rewind commit", §10 Q6 / DEC-77 Q6 (which settles txindex only).

**Core, both pins.** `BlockFilterIndex::Rewind` / `CustomRewind` and `CoinStatsIndex::Rewind` / `CustomRewind` read the height-keyed rows of `(new_tip, current_tip]` and copy them to hash keys before the rewind commit:
- `CopyHeightIndexToHashIndex`: `blockfilterindex.cpp:319-329` v23 / `:341-349` v24; `coinstatsindex.cpp:261-271` v23 / `:264-272` v24.

The FTs depend on it:
- `rpc_getblockfilter.py:44-48` (v24) asserts a filter for every **stale-chain** block after a reorg;
- `feature_coinstatsindex.py:287-288` asserts that `gettxoutsetinfo(hash_or_height=<reorged-out hash>)` still returns the right stats.

Under the design, a rewind commit or the path drain's disconnect leg deletes the height rows. Nothing writes the hash-keyed copies, because the only rewind hook is blind and runs **before** the per-block `stage_disconnected`. So both FTs fail. Coinstatsindex cannot even recompute the copy in `stage_disconnected`: its `DBVal` at height h is the running state, which lives only in the row that was just deleted.

**Why a read is allowed here.** ST3a item 13 forbids reads of keys **that may hold pending writes**, because those force lazy bits. Rows at heights ≤ `h(L)` are barrier-covered: the locator's scope includes all data through L, submitted and synced before the locator. Reading them forces no bit. Only `(h(L), h(L) + D_max]` is unsafe to read.

**Fix.** Replace `rewind_ranges` with `stage_rewind(&self, fork: u32, l: u32, hi: u32, view: &DurableBelow<'_>, batch)`. It may read rows at heights ≤ `l` (the API enforces the bound) to stage hash-keyed copies, and it deletes `(fork, hi]` blindly. In the path drain, copy rows above L from the subscriber's in-memory or staged state, never from the DB. Add C8 cases asserting that stale-block data is retained, plus the two FTs to IX1's list as CH2-dependent. ST3a's R11c reference must model the hash-keyed copies; that is a one-line ST3a erratum, alongside the Q6 one.

### F11 (major): the CH2b phase order cannot meet its own acceptance criteria

**Where:** §7.2 b1/b2/b4.

**Why.**
- b1 (Flush) lists C1, C2, C5–C7, C9 "exhaustive", and T1–T4, T6, T12, T14, T17, T18, T20, T21, T24, T26, T29, T34 "caught". Every CRASH check (R0–R6) runs after `Sut::open`, which is **Recovery**, and Recovery is b2. T3 (R3), T4 (R0), T7 and T17 all need recovery to report.
- ST3a's scenarios for these bugs (S1, S4, S4b) all include a **synced subscriber** (`ST3a-power-loss.md` §6.2 table), and that is b4's driver.

So b1 cannot pass as written, and b2 inherits half of b1's list.

**Fix.**
- Merge b1 and b2 into one phase ("flush + recovery, subscriber-free"). Run ST3a's scenarios in `subscribers: None` variants, with ST3a's owner confirming that each listed T is still caught without a subscriber (T29 needs only a presumed effect; T14 needs R12).
- Move every subscriber-bearing scenario (S1 as written, S2, S4, S4b with subscriber, S11*, S13, S14) to b4.
- b3 (prune) stays subscriber-free, using S5, S5b, S5c, S14b and the selection test. S14 needs a synced subscriber, so it moves to b4.

### F12 (major, cross-task): legacy-undo normalisation as written writes into the hard-linked source rev files

**Where:** §3.5.2 step 3 ("writes a current-format undo record into rdashd's own rev files through ST2's `write_undo` … the hard-linked source file is left untouched"); the CH5 and CH6a row texts in §7.4; DEC-77 Q3.

**Why it fails.**
- ST2's `BlockFiles::write_undo(undo, file, height, prev_hash)` appends to the rev file **paired with the block's blk file** (`flatfile/blocks.rs:437-481` on `node/integration`).
- Core's index format has a single `nFile` for both block and undo (`CBlockIndex`, `FindUndoPos(…, pindex->GetBlockPos().nFile, …)`), and our records mirror it.
- CH6a's safe import **hard-links sealed files** (PLAN CH6a; R10 LinkRule).

So the normalised record lands in an inode shared with the user's Core datadir. That breaks CH6a's "source files unchanged" acceptance, and with it ST3a's R10.

On mainnet, every rev file from the pre-v0.12.2.2 era is affected, roughly the first ~0.8 M blocks.

**Two smaller gaps in the same paragraph:**
- **Stale blocks.** "Resolve each txid's height with one pass over the adopted blocks" is ambiguous for stale-branch blocks, which also carry undo. The same tx is routinely mined at different heights on two branches. Resolution must follow the block's own ancestry. Alternatively, drop `HAVE_UNDO` on stale legacy blocks instead of normalising them: after a full `-reindex`, Core has no undo for them at all, so "byte-identical to Core after `-reindex`" does not apply to them.
- **Finalized rev files.** Appending into a finalized rev file interacts with ST2's BL-29 finalize rule; CH6a must reopen and re-finalize.

**Fix.** CH6a copies (never hard-links) every rev file that will receive a normalised record, or writes the normalised records for such a file into a fresh rdashd-owned rev file and rewrites the affected undo positions. State the ancestry rule for resolution. Add a stale-branch legacy block to the Q3 fixture.

The core of ruling 2 is sound: reachability is only through imported rev files, and recovery refuses zero heights and stays blind.
- rdashd never writes height-0 undo, because genesis outputs never enter the set.
- A full `-reindex` rewrites undo, because the index is rebuilt with null undo positions.
- `-reindex-chainstate` keeps the positions, so it inherits the import's normalisation.
- An `invalidateblock` + crash on an imported datadir reads only normalised records.

### F13 (minor): fallback layout is internally inconsistent

`ShardedLayer` carries `dirty_order: Vec<OutPointKey>` (§2.3). That is 36 B per dirty entry on top of the 73 B bucket, and it is not in the ≈ 115.7 B parity arithmetic. It also contradicts "its iteration order depends on the salt".

**Fix.** Either drop `dirty_order` and iterate the shards (salted, fixed under the seam), or count it and redo the parity claim.

### F14 (minor): the manual prune (`pruneblockchain`) must be synchronous; the policy table makes it "pending"

Core's `PruneBlockFilesManual` → `FlushStateToDisk(NONE, height)` (`validation.cpp:4594 / :4980`) prunes and unlinks before the RPC reads `GetFirstStoredBlock` for its return value. `feature_pruning` asserts those return values.

§3.3 row PRUNE says "F in flight → pending; D recomputed at the next decision". A related gap: deferring a prune flush must not clear `m_check_for_pruning` (Core clears it only when it actually selects, `:2809 / :3017`).

**Fix.** Manual prune = `WaitThenFreeze` + `wait_done`, replying after step 12. For automatic checks, a deferred decision keeps the check armed.

### F15 (minor): shutdown order and subscriber deregistration are unspecified for step 6

Core flushes twice at shutdown:
1. `ForceFlushStateToDisk`, with the indexes still running (`init.cpp:345 / :383`);
2. `FlushBackgroundCallbacks`;
3. index `Stop()` (`:383-390 / :421-440`);
4. a second `ForceFlushStateToDisk` (`:403 / :453`).

If ND1 mirrors this and the second ALWAYS ever freezes a non-empty active, W's step 6 waits on stopped subscribers. §4.2's "active is empty and the tip equals `meta.tip`" shortcut makes that unlikely, not impossible: clean prefetch copies make active non-empty.

**Fix.** `SubscriberHub::deregister`; step 5 never captures a deregistered subscriber; spell out the shutdown order for ND1. Use "active has no DIRTY entry" as the shortcut condition.

### F16 (minor): the layout gate's latency half is wall-clock on a noisy box

The latency criterion is "hit latency ≤ 1.1× the fallback's" (§2.3, §10 Q5). ST1 saw within-cell spreads of up to 620 %. Bytes per coin is deterministic; ns per hit is not.

**Fix.** Gate on PMU counters, which work on agentbox (memory note `agentbox-perf-counters`): instructions plus LLC misses per `get`. Or use ST1's paired rotations with a sign test.

### F17 (minor): `StableBytes` as sketched is likely Stacked-Borrows UB

`UnsafeCell<Vec<Box<[u8]>>>`, pushed through `&self` while `&[u8]` borrows into earlier boxes are live (§2.4.2). When `Vec` grows it moves the `Box`es, and moving a `Box` asserts uniqueness over its allocation. That invalidates the outstanding borrows under Stacked Borrows, which is the very Miri run the design relies on.

**Fix.** Store `NonNull<[u8]>` from `Box::into_raw` and free them in `Drop`, or use a chunked byte arena that never moves a chunk.

### Nits

- **N1.** §4.3 "Why replay converges" says F's dirty keys ⊆ K, the net path's keys. That is false for keys touched off the net path inside the window. Example: X connected then disconnected; X's inputs come back DIRTY with their original values. The conclusion survives, because F's value for such a key equals its unchanged value at `old`. Restate the argument as "for every key, F's value is its value at `new`; keys off the net path have equal values at `old` and `new`", and make sure `coins_replay_blind` generates connect-then-disconnect windows.
- **N2.** §2.9 and §8 row 44 cite `m_synced` at `index/base.cpp:77 / :89`. Those lines are comments; the assignment is at `:78 / :90`, as ST3a had it. The reconciler's "correction" is wrong.
- **N3.** S is defined from B, which varies with mempool usage. `write_buffer_size = S` is static. Define S from the coins share, not from B(t).
- **N4.** §4.3 step 1 computes locks at `height(LCA(locator, recovered tip))` before the index is loaded (step 3) and before the tip is known (step 6). Say "registered at step 1, evaluated before the first prune" (ST3a §5.5 has the same wording).
- **N5.** `wipe_chainstate` resume must run before step 4a's "no marker ⇒ chainstate empty" check, or a crash between the wipe's steps 1 and 2 reports `Corruption`.
- **N6.** V's idle loop (at the tip, no blocks) must wake on `FlushDue`. §4.4 mentions "next poll or current wait" only.
- **N7.** Core's budget counts `CoinsTip()` only (`:2743 / :2951`), not the child view. Counting the overlay inside B makes rdashd flush slightly earlier at tiny B. That is harmless (D1), but say so.
- **N8.** IX1 should also note that v24's eager rewind (F2) changes **when** stale-block hash copies are made (F10), so getindexinfo-style queries mid-reorg differ per profile.

## Verified with no finding (threaded design, recovery, budget)

- **Prefetch fence** (§2.4.3). Every stale result is dropped at the release that ends its generation, and only one freeze happens per release. Any key changed in active after a snapshot is shadowed by active, or by the next F, which is consulted before prefetch. Positive-only admission rules out resurrection through absence. Sound.
- **F-over-DB = DB at `new`** (§3.2), the D2 FRESH deviation, and both defensive checks hold. Checked case by case against `AddCoin`, `SpendCoin` and `BatchWrite`, including tombstone-in-F + FRESH re-add, then spend, release and re-freeze.
- **Blind undo and forward legs** reproduce `DisconnectBlock`'s and `RollforwardBlock`'s last write per key, given the stated op order: reverse tx order; outputs before inputs; inputs in reverse order. v23's BIP30 exception hashes (`:2039-2040`) are Bitcoin's, irrelevant to Dash; v24 hard-codes `fEnforceBIP30 = true` (`:2321-2322`).
- **The presumed-effect ledger and `PreviousFinalizer`** (§4.1) meet ST3a §2.4.4 with returned-before-started ordering. Effects after the freeze are not counted.
- **§4.4's per-subscriber-thread case** (apart from F9): the drain checkpoint only waits on the subscriber's own engine; W reaches B1 without subscribers; control events bypass the bound; V's waits service `PruneClears`, and W never holds the `BlockFiles` mutex while waiting on V (rule 4).
- **ST3a §5.6 items 1–13** each map to a concrete component (§4.7). Apart from F3, F5 and F11, every T bug I checked is plantable in CH2 or ST2 production code: T6 is in ST2's barrier; T13, T15 and T19 in step 0/7 placement; T33 and T35 in the driver; T34 in Bk's commit kind.
- **CS5a** already carries the fallible `CoinsView` (17014d4b, `dash-consensus/src/coins/view.rs`), so DEC-77 Q1 is done.
- **ST2's selectors** call `prune_one_block_file` inside the selection (`blocks.rs:580-660`), so the pure split (§2.7, Q4) is necessary.

## Summary by severity

| Sev | # | Finding | Scope item |
|---|---|---|---|
| major | F9 | the shared-events-thread deadlock through an in-flight F's step 6; §4.4's argument is wrong there | 3 |
| major | F10 | blind `rewind_ranges` loses Core's stale-block hash copies; `rpc_getblockfilter` and `feature_coinstatsindex` fail | 5 |
| major | F2 | MPIX-11 (v24 eager rewind) has no home in `SubscriberDriver`; a literal port puts a locator ahead of G | 1, 5 |
| major | F1 | no overshoot rule mid-block with F absent; the asserted invariant is unachievable (`feature_dbcrash` B ≈ 1.75 MiB) | 3, 7 |
| major | F11 | CH2b's b1 acceptance needs b2's recovery and b4's subscribers | 6 |
| major | F3 | X16 is undetectable: the latch blocks the durable evidence | 2 |
| major | F4 | DEC-43 proxy: RssAnon + RssFile is biased by mmap vs `pread`, and no cgroup cap is stated | 7 |
| major | F12 | legacy-undo normalisation appends to hard-linked source rev files; stale-branch resolution is unspecified | 4 |
| minor | F5–F8, F13–F17 | T12 and C11 are CH3a's; the AbortNode text; `have_in_cache`; D7 point reads; fallback `dirty_order`; synchronous manual prune; shutdown deregistration; layout-gate noise; `StableBytes` UB | 1, 2, 5, 6, 7 |
| nit | N1–N8 | the convergence wording, `m_synced` lines, S definition, lock-load order, wipe order, idle wake, overlay in budget, IX1 note | — |

**Strengths.**
- Core equivalence is unusually careful. I re-derived every flush trigger, the BatchWrite marker protocol, the ReplayBlocks strings and the cache state machine at both pins, and the corrections hold. The three "Failed to …" strings really are unreachable; my only correction is the user-visible text (F6).
- §3.2's equivalence argument (active is Core's tip right after a flush) is the right abstraction and survives adversarial cases.
- The reconciliation choices in §8 are well reasoned.

None of the majors needs an architectural change. Each is a local rule, an API extension or a re-phasing.

## Verdict

**APPROVE-WITH-CHANGES.** Before implementation starts:
- address F1, F2, F9, F10 and F11 in the design text;
- address F3 and F4 in the acceptance and gate text;
- carry F12 into the CH6a/CH5 row text, and amend DEC-77 Q3's mechanics.

The minors can be folded into the same revision.
