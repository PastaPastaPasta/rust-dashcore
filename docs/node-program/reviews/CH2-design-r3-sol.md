# CH2 design rev2 — round-3 closure review (Sol)

**Verdict: CHANGES-REQUIRED.** The prior visibility, filter-position, keyed-index and pending-emission sequence findings have concrete design-level fixes. The memory admission/completion proof is still incomplete, and the event-source contract omits the synchronization that the model assumes when a subscriber switches from catch-up to live events. These are gate findings; under DEC-92, the sequential Opus pass should wait for their closure.

Reviewed `designs/CH2-coins-flush.md` at **76391f21c66f7081fc900a204116b02760fff10d**, including the step-1 change at **0c7abf97** and step 2 at **4cadb648**. Read “Changes in rev2” first, all four supplied r1/r2 reviews, the normative design, and DEC-78/80/81/82/85/92/94. DEC-94's Q1–Q7 policies, the Linux accounting reference, D12/D13 withdrawal and CH2b's SB0 hold are binding. Findings below concern whether the stated mechanisms implement those decisions, not whether those decisions should change.

Inputs were read from `~/workspace/rdc-wt/integration`; its HEAD was `4281327b` when this review began. The pinned design was byte-identical to the checkout. Core citations below are actual `git show v24.0.0-rc.3:<path>` blobs from `~/workspace/dash` (tag resolves to `da567d7dba945143eee4d2ce1621c21b32089989`). No source, supplied model, design or decision was edited. This report is the requested output exception. No commit, push, PR, node instance or datadir operation was performed. Scratch was `/work/scratch/ch2-r3-sol/` and was removed after recording the evidence here.

## Prior-item status

**CLOSED** means closed at the design level; it does not certify the future implementation. **PARTIAL** means the specified fix resolves part of the finding but still has an actionable residual below. Line references are to the pinned rev2 design.

### GPT round 1 and round 2

| Prior item | Status | Evidence / residual |
|---|---|---|
| G-R1-B1: already-reached flush never acknowledged | CLOSED | §4.4.1 case A commits/acks before later data; queued control copy is a no-op. Exact directed rerun: 122 states, 3 completion states, no deadlocks or states unable to finish; X18 fails. |
| G-R1-M1: no block completion under hard ceiling; release race | PARTIAL | Rule O is an authorized completion policy and W drops its Arc before `FlushDone` (§4.2 step 11). Its admission calculation and the fixed-capacity fallback still lack a valid completion proof: G-R3-M1–M3. |
| G-R1-M2: claimed hard engine-memory ceiling | CLOSED | §2.5 explicitly makes E a configured target, uses non-strict LRU/stalling WBM/no L0 pinning, and assigns native diagnostics plus the common-cap `rss_anon_peak` gate. DEC-80/94 settle that policy. |
| G-R1-M3: no state at locator target X | CLOSED | §2.9 items 2 and 5a retain state at each X in `(L,p]` and atomically write its encoding with X; C8(d) exercises an accumulator with X behind p and crash/rewind points. |
| G-R1-M4: legacy undo resolved on wrong branch | CLOSED | §3.5.2 retains spending-block/transaction/input context, selects creating occurrences by ancestry and in-block order, and specifies missing/ambiguous active and stale cases; two-branch fixture retained. |
| G-R1-m1: hidden-freeze membership/uncache | CLOSED | §2.4.5 separates logical Core membership from physical active/F membership; own freezes do not clear the shadow, and `Uncache` updates it. §6.2 now compares logical membership in both L1 modes. See D12 assessment below. |
| G-R1-m2: locks computed before index/G load | CLOSED | §4.3 steps 3–6 load file metadata/index, choose G, then register ancestry-derived locks before pruning/activation. Wipe resumption precedes marker checks. |
| G-R1-m3: VARINT described as numerically ordered | CLOSED | §2.2 correctly says serialized-key order and records 16383=`fe7f`, 16512=`808000`; §6.1 gates that cursor vector. |
| G-R2-M1: long fetched/restored input coins omitted | PARTIAL | Long input bytes are now recognized and the historical-script size limit is correct. Enumeration, streaming admission and the bound do not yet form a valid proof: G-R3-M1–M3. |
| G-R2-m1: atomic reorg exceeded modeled Q | CLOSED | `v_moves` has separate tip-change and pending-emission steps, one disconnect per step; `invariants` asserts Q in every reached state. The rerun includes depth-2 reorg and invalidate modes at Q=2. |
| GPT r2 note: fairness / AG EF limits | CLOSED | §4.4.2 states finite I/O and weak fairness; §4.4.3 explicitly distinguishes reachability of completion from exclusion of unfair infinite runs. |
| GPT r2 note: coin-empty changes and round trip to last frozen tip | CLOSED | §4.4.1 rule 5 always honors a pending `FlushDue`, using an empty flush when needed; it disables the index-only shortcut in that case. |
| GPT r2 note: parked subscriber + later disconnect | CLOSED | Wake on enqueue plus compression; directed unplanted state completes in 5 states. No-wake fails; no-compression alone succeeds with the empty flush; the combined no-compression/no-empty plant fails. |

### Opus round 1

| Prior item | Status | Evidence / residual |
|---|---|---|
| F1: mid-block budget/completion | PARTIAL | Same residual as G-R2-M1; rule O and release ownership are fixed, but G-R3-M1–M3 remain. |
| F2: MPIX-11 / unsafe eager locator | CLOSED | §2.9 separates reported best from durable L, rewinds reported best on indexed-chain disconnects, caps eager forward commits at `LCA(pprev, commit_cap)`, and keeps Durable backward rewinds. C8(b),(i) carry acceptance. |
| F3: X16 invisible behind fail-stop latch | CLOSED | X16a and X16b have distinct plant sites; FA7 checks fatal return and forbidden outcomes from the fault through epoch end, rather than relying only on durable invalidity evidence. |
| F4: biased RSS and unspecified cap | CLOSED | §5.5, W-MC §5.8 and CH2c/M3 row text §7.4 consistently gate `rss_anon_peak` under a common cgroup covering heap and page cache, with no swap. |
| F5: CH3a bugs tested only in stub connect | CLOSED | §6.4/§7.2 remove T12 and connect halves from CH2; §7.4 explicitly adds T12, C11 connect half and T20/T21 connect halves to CH3a acceptance using real `connect_block`. |
| F6: AbortNode user text | CLOSED | §3.6 retains the `***` log prefix, generic dialog, misc warning and v24 status 1, matching `shutdown.cpp:27–36`. Residual v23 wording is a nit below. |
| F7: first hit could be spent | CLOSED | Superseded by §2.4.5: `member` requires an unspent coin; physical tombstones remain authoritative for values. |
| F8: corrupt point-read deviation undeclared | CLOSED | D7 covers point reads, cursor failures and trailing bytes; a3 tests guarded point/snapshot/multi-get/cursor paths; DEC-78 remains binding. |
| F9: shared events thread blocks other subscriber's ack | CLOSED | Limit handling parks without retaining a thread; both shared and per-subscriber configurations pass the rerun; X17 is caught. |
| F10: stale-block archives lost | CLOSED | Above-L sources come from the journal and are written on disconnect after file sync; below-L sources come from `DurableBelow`. C8(c) requires post-reorg filter readback and stale coinstats. |
| F11: impossible phase acceptance ordering | CLOSED | b1 combines flush/recovery without subscribers, b2 adds pruning, b3 adds subscriber-dependent scenarios; owner confirmation of the `None` variants remains an explicit prerequisite. |
| F12: normalization writes hard-linked source; ancestry gaps | CLOSED | §3.5.2 uses temp/new inode → file sync → rename → directory sync → updated index positions; source hashes are gated across import/use/restart. Active failure/stale clear rules are explicit. |
| F13: fallback dirty-order memory missing | CLOSED | §2.3 removes the side list and iterates shard/table order with fixed seam salt. New fallback completion issue is G-R3-M3. |
| F14: asynchronous manual prune / lost check | CLOSED | §3.3 waits for release then returns only after unlink/directory sync; deferred automatic checks stay armed. |
| F15: shutdown capture of stopped subscribers | CLOSED | Hub deregistration plus two-flush shutdown order in §4.2; second flush captures no stopped subscriber; shortcut uses no DIRTY entry. |
| F16: noisy latency layout gate | CLOSED | §2.3 gates PMU instructions and LLC-load-misses and confirms with paired wall-clock rotations, as DEC-80 directs. |
| F17: moving Box invalidates scratch borrows | CLOSED | `StableBytes` stores `NonNull` from `Box::into_raw`; vector growth moves raw pointers, not Boxes; Miri under both borrow models remains acceptance. |
| N1: off-net-path replay keys | CLOSED | §4.3 convergence proof now states every F value equals the value at new, including unchanged keys outside the net path; fuzz generates connect/disconnect windows. |
| N2: m_synced citation | CLOSED | `index/base.cpp:90` is the init assignment; `:169` is the transition after final sync commit. |
| N3: S depended on changing B(t) | CLOSED | §2.5 derives S from the static coins share. |
| N4: startup lock evaluation too early | CLOSED | §4.3 step 6 follows loaded index and selected G. |
| N5: wipe resume after marker check | CLOSED | §4.3 step 2 resumes wipe before marker checks. |
| N6: V idle loop must wake | CLOSED | §4.4.1 rule 5 explicitly wakes on `FlushDue`. |
| N7: overlay counted unlike Core | CLOSED | D11 explicitly declares the earlier tiny-budget flush caused by counting overlay/T. |
| N8: eager rewind affects archive timing | CLOSED | §2.9 uses v24 disconnect-time archiving/eager commit; §2.11 records the archive visibility dependency. |

### Opus round 2 and the additional author claims

| Prior item / claim | Status | Evidence / residual |
|---|---|---|
| O-R2-M1: rows invisible at reported best | CLOSED | §2.9 item 1 writes height/inexact rows in one Buffered block batch before advancing best. Keyed `read`/`iter_prefix` overlay newest journal operations, including deletes, over DB; C8(g) exercises no-flush queries. |
| O-R2-M2: filter position rewinds | CLOSED | `latest_high_water` survives journal pops and every backward state move; archive writes follow `sync_files`; open floors position by durable state and file length. Matches the intent of `blockfilterindex.cpp:296,309,353–356`; C8(c) tests stale bytes after replacement connects. |
| O-R2-M3: IX2 has no row class/undo path | CLOSED | Keyed rows stay only in locator batches/journal; `stage_disconnected` computes inverses from block/undo, with top-down rewind order. §2.9 item 8 and C8(h) carry address/spent/timestamp tests. v24 address rewind handles intra-block spends in reverse (`addressindex.cpp:306–365`). |
| O-R2-M4: 1024-block forced-flush cliff | PARTIAL | No block-count limit for inexact-only txindex; proxy includes it and reports causes. The 0-DMAX result still needs the byte-limit exception/compaction specified in G-R3-m1. Core's roughly 10–15 count is explicitly an estimate, not a verified full-window measurement. |
| O-R2-M5: equal-shard cliff / arbitrary gate points | PARTIAL | Full-budget sweep, mainnet length histogram and Core floor close the original gate measurement problem; presizing removes rehash cliffs. Its new fixed cap cannot complete oversized operations as written: G-R3-M3. |
| O-R2-M6: freeze before Connected(new) emission | CLOSED | `tip_seq` assigned before waits and carried in packet/control; case B uses `seq_synced`, `last_seq`, `skipped`; queued copy is always a no-op. Exact directed configuration: 834 passing states; queue-membership plant has 797 states unable to finish. New handoff issue G-R3-M4 is a different race. |
| O-R2-m1: growth omitted from T | PARTIAL | Item e now names boundary reservation and overlap, but the final `T_max` expressions do not bound it or item f: G-R3-M1. |
| O-R2-m2: F4 row/W-MC mismatch | CLOSED | §5.8/§7.4 now consistently say `rss_anon_peak`, with RssFile/cgroup peak reported only. |
| O-R2-m3: empty FA7 window | CLOSED | FA7 now says “from the fault until the epoch ends.” |
| O-R2-m4: D12 verdict rationale | CLOSED | The rationale correctly cites the verdict choice at `validation.cpp:970–979`; shadow is the selected mechanism, and D12 withdrawal is DEC-94's ruling. |
| O-R2-m5: D13 wording | CLOSED | §2.9 item 5b explicitly specifies the capped eager commit and its legal comparison with Core; D13 withdrawal remains binding. |
| O-R2-m6: above-L archives lost on process kill | CLOSED | Item 4 writes them immediately Buffered after `sync_files`; `Commit::Buffered` promises process-crash survival on return in `dashd-store/src/engine.rs`. |
| O-R2-m7: coin-free freeze / ALWAYS shortcut | CLOSED | Tip changes and an empty-flush fallback cover both cases; directed no-empty plant is correctly qualified as paired with no compression. |
| O-R2-m8: model emission fidelity | CLOSED | One disconnect per step, separate pending emission, freeze during emission wait, and Q invariant are implemented. See additional model limits below. |
| O-R2-m9: unguarded snapshot/cursor | CLOSED | §2.6/§2.10 require `GuardedSnapshot` and guarded table handles; a3 acceptance includes those fault/decode paths. The API sketch still has older result types: G-R3-n1. |
| O-R2-m10: journal memory unbounded | PARTIAL | Byte budget and removal of txindex data rows are progress. Per-block byte admission and the inexact-only metadata case still need an implementable rule: G-R3-m1. |
| O-R2-n1: CH3a acceptance row | CLOSED | Explicit acceptance edit in §7.4, not merely a dependency change. |
| O-R2-n2: missing packet sequence | CLOSED | `FreezePacket.tip_seq` at line 855. Older `connected_seq` spellings in the step table are a cleanup nit. |
| O-R2-n3: state init may need L's height row | CLOSED | `decode_state(..., at_l)` and item 7 allow only barrier-covered height reads at/below L. |
| O-R2-n4: no per-height prefix scan | CLOSED | `DurableBelow::height_rows` supplies it. |
| O-R2-n5: rewind commit trigger | CLOSED | Item 5c explicitly triggers at next connect, request or flush commit. |
| O-R2-n6: RULE_O cause / prefetch overshoot | CLOSED | Cause declared and oversized path clears prefetch. Fixed table/shadow allocations still need proper baseline accounting in G-R3-M1–M3. |
| O-R2-n7: trailing bytes | CLOSED | D7 expressly requires consuming the entire stored value and declares Core's permissive behavior. |
| D12 withdrawal / Core-membership shadow | CLOSED | Logical membership rule covers created-since-epoch, fetched-unspent and restored coins, with spend/uncache/Core-flush maintenance and below-epoch reconnect heights. Own physical flushes do not change logical membership. Exact Linux meter/oracle validation is an implementation obligation, not established by the liveness model. |
| D13 withdrawal / no Q10 lag | CLOSED | If old L is an ancestor of X, `LCA(old L,new)` is an ancestor of or equal to `LCA(X,new)` for any future new. Thus raising L cannot lower the stated real prune lock. Backward rewinds have the separate pre-B1 gate. |
| Model grid and planted bugs | CLOSED | Inspected supplied full-grid logs, independently summed counts, reran a substantial subset and all directed checks, and reproduced X17/X18/X19/X22. X20/X23/X24 qualification is accurate; X21 is a corruption test, not part of this Python model. |
| SubscriberFeed F1–F9 / P1–P9 sufficiency | PARTIAL | The existing live-stream rules cover ordering, queues, wakes, controls, data and barriers. They do not explicitly give the atomic snapshot-to-live handoff assumed by `syncing_step`: G-R3-M4. |
| EngineTuning isolation | PARTIAL | Configuration/open, post-Bk, diagnostics, journal sizing and delete-span choices have adapter homes. `CoinsBudget::reserve` still hardcodes the Rocks reserve instead of taking the adapter's value: G-R3-m2. Durability ordering itself uses the engine contract. |

## New findings

### G-R3-M1 — MAJOR: the allocation list and `T_max` proof still disagree

**Where:** §2.5 items c/e/f and lines 663–687; §2.3 layouts; §6.7 assertions.

The arithmetic of the printed expressions is correct:

| Expression | Bytes | MiB |
|---|---:|---:|
| `48,780 * (2*84 + 10,048) + 2*262,144` | 498,860,768 | 475.7507019043 |
| `48,780 * (84 + 10,048)` | 494,238,960 | 471.3430023193 |

The 41-byte input and 9-byte output minima are conservative upper-count bounds; the 2 MB block cap and 10,000-byte maximum stored script are correctly cited. Counting historical input scripts removes the old tenfold mistake. **These facts do not prove that the expressions bound allocations (a)–(g).**

1. **Item c undercounts long output storage.** It charges only `len(coin)-34` per copy. `PackedCoin::Long(LongRef)`/the fallback Box retain the whole contiguous serialized coin, as required by `as_bytes()`; the slot's unused inline area does not replace the first 34 bytes of that allocation. A 40-byte long coin has a 40-byte payload allocation, not a 6-byte allocation. Two copies miss 68 bytes per long output, before allocation overhead. This directly invalidates `T_known(op)` even on a block with no fetched long inputs. Either charge the full long allocation/capacity or specify an actual split representation with its contiguous-read scratch included.
2. **The fixed maximum silently omits e and f.** Reserving growth at the boundary makes it legal to decide before growing; it does not make the growth free. Item e depends on the existing table size. At the allowed 16 GiB cache setting, growing an arena index from `2^26` to `2^27` buckets allocates **640 MiB for the new 5-byte-per-bucket table alone**, while the old index still exists. Its approximately 58.7 M inline entries and old index occupy about 4.25 GiB, so this growth can be reached before the f=1/2 freeze threshold. Even the DEC-43 `2^21 -> 2^22` growth is 20 MiB of new index allocation, not either of the two 256 KiB constants. Item f's set/map reserve and overlap have no term in either expression. The fallback formula additionally has only one `84` term although its table lists two entry classes; explain which cost is an already-allocated baseline and which is incremental.
3. **Other allocation ownership is not fully specified.** A DB read returns an owned `Vec<u8>` under `KvRead::get`; conversion into an arena payload can overlap that read buffer. Restored long coins need an explicit borrow/move/copy path from undo through child to active to justify counting their bytes once. `free`, chunk-pointer vectors, long-arena slack and shadow-set capacities need concrete peak bounds, not just a list of allocation sites. A post-abort eviction may remove entries without freeing their arena/index capacity.

DEC-94 approves an approximately 476 MiB **oversized-operation** allowance. It does not imply that *every ordinary operation's reservation at every existing table size* is below that number. The design currently asserts `T_true(op) <= T_max` for every op (line 702); that is false for the stated growth term. Separate boundary maintenance/headroom from the empty-layer oversized envelope, or prove the invariant over both. Define the actual baseline, including a presized empty table and retained shadow allocations. Derive a ledger that matches the representation and keeps the authorized allowance; a unit test that only recomputes the printed multiplication is not a proof.

**Closure:** full-length long-output cases; restored-coin ownership; actual rehash overlap at DEC-43 and maximum cache sizes; set/map growth; read-buffer/arena overlap; and the oversized empty-layer path must pass the tagging-allocator assertion. Include a numerical bound for every term.

### G-R3-M2 — MAJOR: streaming admission does not prove an ordinary block stays in B or needs at most one retry

**Where:** §2.5 lines 669–708; §6.7.

The admission test uses `usage > max(B, U0 + T)` while adding each fetched allocation to T as it arrives. If T includes those allocations, this compares the growing usage against a ceiling that grows with that usage. It does not detect crossing B on an attempt initially admitted as ordinary.

An independently transcribed arithmetic witness (MiB units) is:

```text
B=4, U0=3, T_known=0.125: ordinary admission succeeds.
Fetch 2 MiB of previously unknown long input coins.
T_measured=2.125, usage=5.125.
usage > B, but usage > max(B,U0+T_measured) is false (5.125 > 5.125).
```

This violates the next assertion that `U0 + T_true > B` occurs only for an oversized attempt. Reserving the *next* coin before reading/copying it does not resolve the moving-threshold problem.

Even interpreting the intended abort test as `would exceed B` leaves the one-retry argument incomplete. The first abort measures only the input **prefix**, not the remaining inputs. For example, start with B=4, U0=3, known overhead about 0.1; the first 1 MiB long input crosses B. After a boundary flush, retry with the measured approximately 1.1 MiB fits an empty cache. A later previously unread suffix can add another 4 MiB, requiring another abort or an unclassified overshoot. The text neither scans that suffix before admission nor says to reserve a conservative complete bound after the first abort. A source change from cached/F inputs to DB after flush can also change physical allocation costs.

**Closure:** use a fixed B ceiling for ordinary attempts and establish a complete reservation before retry. A bounded-memory preflight over all remaining inputs, or a conservative reservation covering every unread input, can do this. Specify what is retained/evicted, how capacities are reclaimed, and why the retry reservation still holds after freeze/release. Prove that no allocation crosses its ordinary reservation, that at most one abort occurs, and that every valid block completes. Test an inexpensive prefix followed by an expensive unseen suffix; identical long inputs alone do not establish the claim.

### G-R3-M3 — MAJOR: the presized fallback has no oversized-operation completion path

**Where:** §2.3 lines 390–392; rule O step 4; a1 deletes the losing layout before a2 tests rule O.

The fallback is never rehashed, is sized from ordinary B, and declares itself full when *any* shard reaches its load limit or allocated long bytes hit its fixed `cap`. Rule O allows an oversized block to finish on an empty active layer, but does not resize that layer or override its entry/byte capacity.

The already-required low-budget witness is sufficient. At `-dbcache=4`, txindex off and no allowance:

```text
B = 1.75 MiB = 1,835,008 bytes
R = B/8 + 64 KiB = 294,912 bytes
cap = (B - R - B/16)/2 = 712,704 bytes
50,000 active slots alone = 3,600,000 bytes
```

A block with 50,000 small spendable outputs fits the 2 MB consensus limit. It cannot fit that active table, even ignoring child overlap, hash/control bytes and long payloads. Freezing a partly applied block is forbidden, waiting with no F frees nothing, and freezing/recreating another ordinary-capacity layer repeats the failure. Long input/restoration blocks present the same problem for the byte cap. Lowering accounting headroom does not create missing physical buckets.

This is a new incompatibility introduced by the step-1 fix, not a rejection of the full-budget layout gate or the authorized overshoot policy. The design predicts that this fallback is likely to ship, so it cannot be dismissed as unused.

**Closure:** before the oversized attempt, construct a transactionally sized exceptional layer (and child) with enough per-shard capacity and full accounting, or specify another bounded completion mechanism. Handle a full individual shard even when total free space exists. Keep normal layers presized if desired; state exactly how the exceptional layer is sized/retired and why its peak fits the authorized envelope. Run the 50,000-output and long-input/restoration cases against the layout that a1 actually selects.

### G-R3-M4 — MAJOR: F1–F9/P7 need an atomic catch-up-to-live handoff, not just an atomic class/sequence assignment

**Where:** §2.9.1 F1/F2/F8; §10.1 P7; `SubscriberDriver::sync_step`; model `syncing_step` lines 276–285.

`ChainSnapshot` is a consistent read-only snapshot. The interface does not require it to remain current, couple its generation to the feed's subscription cut, or revalidate its tip when switching to Synced. F2/P7 make assigning `seq_synced` atomic with the class transition, but do not explicitly make **validating the completed snapshot's tip against live V and installing the event subscription** part of that atomic operation. “As Core's m_synced” is not an implementable substitute for this shared-base contract: Core performs its final `NextSyncBlock` test/commit/class transition under `cs_main` (`index/base.cpp:162–170`).

A producer can satisfy the enumerated live-event guarantees while doing this:

1. A Syncing subscriber completes a consistent snapshot ending at A3, with durable L=fork2.
2. Before its final capped commit/class transition, V disconnects A3 and connects B3. While the subscriber is Syncing it is not receiving that live feed; those changes are absent from the eventual subscription.
3. The subscriber commits the safe cap fork2, atomically marks Synced and learns the correct *current* next sequence. It starts a lossless, ordered, bounded feed from that cut. Every subsequent event has readable data/undo, out-of-band controls and the proper wake/barrier behavior.
4. `Connected(B4)` arrives. The driver still has p=A3, so it ignores this nonconnecting event. On `FlushCommit(B4)`, `last_seq` says the event was processed/ignored, so case A acknowledges at fork2. Later B-branch connects are also ignored; a static tip gives no new events to repair p. `best_block()` truthfully reports the driver's old A3 forever.

F1's guarantee needs a defined starting chain state as well as ordered events after the start. “No loss” must expressly cover every tip change between the catch-up snapshot's terminal sequence and the live subscription, not merely events generated while Synced.

I transcribed the post-handoff state into the supplied rev2 transition functions, without modifying that model. They produce:

```text
S0.ignC(4,1)       p=(3,0), V=(4,1)
W.B1((4,1))
S0.commitA@(2,0)   p=(3,0), L=(2,0)
S0.skipFC
```

This is a counterexample to an underspecified **producer/handoff contract**, not a claim that the state is reachable in the original model. That model checks `s.p == st.vtip` and installs the class/sequence cut in one atomic transition; it rules the race out by construction. SB0 must preserve that additional assumption.

**Closure:** give `ChainSnapshot` a coherent `(tip, tip_seq/generation)` and a subscribe/finalize operation linearized with V's tip changes. At its cut, either p equals the live tip or an ordered replay suffix from p to that tip is guaranteed before later events; no tip change can fall in the gap. Define whether an already-pending event is included and ensure `seq_synced` matches that decision. Add this explicitly to F2/F8/P7 and to SB0's input. Extend the model/threaded tests with a reorg between snapshot completion and final subscription, plus a pending emission at that cut. A reconnect-on-ancestor behavior copied from Core also needs its precise handling specified, rather than relying on discarded events to recover.

## Additional changes and nits

### G-R3-m1 — MINOR: byte-budget handling needs an admission rule and an explicit inexact-only exemption/compaction

§2.9 item 2 bounds every journal to `journal_budget` bytes; §4.4.1 rule 3 treats reaching that budget as the same limit that posts `FlushDue(DMax)`. Item 6 simultaneously says txindex retains one `BlockRef` per block and **never** posts that cause. Setting `d_max=None` removes the block-count limit, not the byte limit. Just hash+height require at least 36 bytes per retained block: 1,028,159 blocks require **35.299 MiB**, before `BlockId` and container overhead. This is not evidence that the DEC-43 trace necessarily crosses 32 MiB between its ordinary freezes; it does disprove the universal explanation that schema/no block limit alone makes DMAX impossible under every `EngineTuning`.

Give inexact-only subscribers an O(1) progress representation (no per-block state/archive/keyed rows to preserve), or explicitly exempt/account their journal metadata and justify the bound. For exact subscribers, specify how the *next* entry's variable bytes are reserved before accepting it and how one entry larger than the configured budget completes. At an empty journal p=L, “checkpoint at p” releases nothing; repeating park/flush cannot make an oversized entry fit. A worst-entry bound for each supported `SubscriberData` or an explicit oversized-entry path is needed. The Python model abstracts bytes away and cannot establish this. Add variable-sized entries and this corner case to b3 acceptance.

### G-R3-m2 — MINOR: route the budget reserve through EngineTuning in the authoritative API

The adapter split covers the important engine choices, and I found no new engine-specific durability ordering. Nevertheless `CoinsBudget::reserve` at line 622 returns `S+64 KiB` unconditionally for any in-flight flush, and line 711/A-1 repeats that as the invariant. This conflicts with `EngineTuning::batch_reserve` and `MemoryTuning`'s specified R=0. Make the budget receive the selected reserve (or the tuning object) and use that value everywhere; keep the Rocks number only in `RocksTuning`. Specify `batch_with_capacity`/actual batch peak accounting for the selected engine: existing `KvBatch::size_bytes` is documented as only a lower bound, and the trait's default `batch_with_capacity` falls back to `batch()`.

Copy-point SST-size preparation and performance workloads can still request the generic `KvEngine::flush_memtables` operation, but adapter-sensitive assertions should observe `after_bk`, not require a Rocks-specific flush call. A custom-tuning test with a reserve different from Rocks/Memory would verify that the core budget/flush driver does not read a Rocks constant. This is compatible with DEC-94 Q6 and does not reopen the storage choice.

### G-R3-n1 — NIT: align older sketches, acceptance text and citation wording

- §2.6's table signatures still use arbitrary `impl KvRead`/`StoreError`, whereas the normative paragraph requires guarded handles/`CoinsReadError`; its `cursor`/`multi_get` sketch should express the selected guarded contract.
- §4.2 step 5 still calls the packet sequence `connected_seq`, though the event that made new the tip may be a disconnect and the actual field is `tip_seq`.
- §3.1 still names removed `SubscriberData::{commit_state, reverse}`; the selected hooks are `with_high_water`/`encode_state`/`stage_disconnected`.
- §5.7's density workload still says 1 M/6.4 M fixed-count points and bytes/coin/hit nanoseconds, contradicting the normative full-budget sweep/PMU gate. §5.2 retains the old fallback average in its first row. Update the summaries so implementations cannot follow a superseded gate.
- §3.6 retains “via v23's AbortError (:31)” in a v24-only source row; at this pin line 31 is `if (user_message.empty())`, not AbortError. Remove that clause. §2.11's MP1 `:963–974` citation is cache/uncache *bookkeeping*, not the actual `Uncache` call; describe it that way. History sections may retain their paired citations.
- The model atomically broadcasts pending data only when **all** Synced queues have room; F3 says a full queue never blocks another subscriber's delivery. Clarify whether that refers to already-queued dispatch or independent fanout. Independent partial fanout is not a state the supplied model explores and should be stated as an abstraction/additional test, rather than saying it assumes “exactly” the full producer contract.

## v24 citation and behavior spot-check

I read the addendum diff against `4cadb648` and spot-checked more than the requested approximately 15 references directly from the pinned Core blobs. I did not certify all 132 conversions individually. No dropped v24 policy switch was found in the checked conversion; the handoff and older summary discrepancies above are the qualifications.

| Checked citation | Result |
|---|---|
| `node/caches.cpp:15–42` | v24 address/timestamp/spent/filter shares precede coins; p300 txindex coins=252.75 MiB; p4 txindex-off coins=1.75 MiB. |
| `consensus/consensus.h:11–14`; `coins.cpp:342` | 2,000,000-byte DIP1 bound; output-count scan bound is size divided by serialized CTxOut size. Historical pre-DIP1 limit is smaller, so this upper bound is conservative. |
| `script/script.h:534–537` | Scripts over 10,000 bytes are unspendable and never stored. |
| `coins.cpp:44–59,61–68` | Fetch caches hits; GetCoin copies and returns false for spent. Confirms the fetched/child ownership issue needs an explicit Rust representation. |
| `coins.cpp:70–108,129–150,182–252` | FRESH/spend/child merge rules, including move on erase, match the Core-value contract. |
| `coins.cpp:40–42`; `memusage.h:187–203`; `pool.h:187` | Core measures pool chunks/list overhead/buckets plus script heap; default pool chunk is 262144 bytes. Exact meter acceptance must match allocator behavior, not just average bytes/coin. |
| `validation.cpp:970–979`; `mempool_accept.py:107` | `txn-already-known` versus missing inputs is verdict-affecting; FT expects already-known. D12 fix is necessary. |
| `validation.cpp:2945–2967` | Mempool allowance, LARGE=max(0.9B,B−10 MiB), CRITICAL above B; no strict per-allocation ceiling in Core. |
| `index/base.cpp:162–170` | Final sync decision/commit/class transition under `cs_main`; this is the missing producer-handoff requirement. |
| `index/base.cpp:294–300` | CustomAppend completes before reported best advances. O-R2-M1 is fixed. |
| `index/base.cpp:238–259,308–330` | v24 eager rewind/commit and indexed-ancestor check. `b != p` alone is a stricter driver rule, not a literal transcription of Core; a safe handoff must justify that narrower event domain. |
| `blockfilterindex.cpp:296,309,341–359` | File position advances; rewind writes the current high-water, not a previous block's position. |
| `blockfilterindex.cpp:362–378`; `rpc_getblockfilter.py:37–47` | Height/hash lookup and both active/stale FT cases. Since CH2 deletes height rows on recovery/rewind, IX1 must also try its archive when a height row is absent; literally copying Core's early return would miss it. C8(c)'s actual readback must cover that. |
| `coinstatsindex.cpp:231–233,350–389,394–399` | Per-block rows omit fixed accumulator; initialization reads L's height row; accumulator and locator commit together. |
| `addressindex.cpp:280–395` | Top-down block rewind, reverse transaction order, outputs removed then inputs restored; intra-block spend semantics retained by the keyed inverse design. |
| `txdb.cpp:113–174`; `dbwrapper.h:322–327` | Head marker/old tip/partial-batch crash points; decode exceptions return absent and trailing bytes are not rejected. D7 intentionally differs per DEC-78. |
| `validation.cpp:3092–3117`; `shutdown.cpp:27–36` | Core coins-before-EvoDB order and flush signal, runtime-error mapping, generic fatal dialog/status 1. CH2's stronger ordering remains declared. |

The high-water/archive mechanism is sound at the design level: every archive pointer is published after its file barrier, backward state moves retain the process-wide maximum, and restart floors it above file tails. Keyed rows are safe under subset recovery because no above-L keyed mutation is submitted separately from its Durable locator batch. Height rows above L are expendable; committed state at L and keyed rows at L stay exact. C8 must exercise actual lookups after blind deletion, not merely inspect archive membership.

## Model reruns and their limits

The supplied logs contain **120 successful state-count records totaling 21,656,118** for rev2, and **60 totaling 4,615,988** for Opus's variant; neither contains a FAIL. Those full counts were independently summed from the logs, not fully rerun in this review.

I ran fresh scratch copies with Python 3:

| Run | Configurations / states | Result |
|---|---|---|
| Rev2 grid subset: init SS/SY/YS × shared/per-subscriber × depth-2 reorg/depth-2 invalidate, D=2,Q=2,ACT=3,MAXH=5 | 12 / **1,910,752** | All safety checks and AG EF completion pass; zero deadlocks / states unable to finish. |
| Opus `stepwise_seq.py` subset: SS/SY/YS × shared/per-subscriber, D=2,Q=2,ACT=3,REORG=2,MAXH=5 | 6 / **434,728** | All pass; removing rewind gate catches R11a. |
| Rev2 `--part directed` | 8 positive/negative checks | All expectations reproduced; exact B1, parked injection, pending-emission sequence case. |
| Rev2 planted X17/X18/X19/X22 | Searched the supplied grid until each witness | All four caught; X19 is R11a, others have states unable to finish. |
| Resource/handoff arithmetic/transition probes | Fixed counterexamples quoted above | Streaming moving ceiling, fixed fallback cap, large index overlap and missed-handoff suffix reproduced. These are design transcriptions, not production-code tests. |

Opus's older variant calls its queue-or-pending test “seq-based” but stores block events without explicit global sequence/`seq_synced`/`skipped` state; its successful run independently supports pending-emission handling, not the full newer sequence-cut contract. The rev2 model does represent those sequence fields.

The queued-data bound is now genuinely asserted. Each V change is assigned a sequence before emission, each disconnect is a separate step, and pending events permit intervening freezes. The stated AG EF/fairness qualification is appropriate. The model does **not** establish memory bounds, variable-size journal admission, data/archive lookup correctness, I/O/pruning/shutdown or a non-atomic subscription handoff. Its passing grid therefore does not close G-R3-M1–M4.

For O-R2-M4's benchmark evidence, I read `/work/bench-store/results/wm-l300/leveldb-r0-micro.json`: 453371 blocks, 8209924 creates, 7200446 spends, `coins_budget=255852544` (244 MiB), **2 flushes**, matching the source cited in §5.9. That result is a Core-model harness cell with different budget details, not a measured v24 p300 full-window count. The approximately 10–15 extension is transparently estimated; the accepted stall/bytes gates and real M3 measurement remain the authority.

## Gate disposition

Close **G-R3-M1–M3** before treating CH2a's rule O/allocator invariant as implementable, and **G-R3-M4** before treating F1–F9/P1–P9 as a sufficient SB0 handoff contract. Carry the two minor contract corrections into the same revision. Keep DEC-94's decisions and CH2b's hold unchanged. Then rerun the affected admission/handoff probes and model regressions, obtain Sol closure, and proceed to the sequential Opus review under DEC-92.
