# CH2 design closure review, round 2 — GPT

**Verdict: REWORK.** B1 is closed, as are M2–M4 and m1–m3. M1 is **PARTIAL**: rule O supplies the previously missing completion policy, but its operation-size proof and stated `T_max` are unsound. One new major is recorded below. The liveness model passes its advertised grid and contains the exact B1 schedule, but it is an abstraction with a concrete queue-bound mismatch; its result should not be called an exhaustive proof of the threaded design.

Reviewed `designs/CH2-coins-flush.md` **rev1**, commit **989aec57c7d89ba2501021b8230db9db7fe991a2** on `node/integration`. Read “Changes in rev1” first, then both round-1 reports, the normative sections and the Python model. **DEC-80 Q1–Q8 and DEC-78 are binding**; this review does not reopen their policy choices.

Read-only: sources and supplied model unchanged. This requested report is the output exception; it was created early and completed after verification. Scratch: `/work/scratch/ch2-r2-gpt/`; `TMPDIR=/dev/shm/ch2-r2-gpt-tmp`, mode 0700. No commits, pushes, node instances or chain datadirs.

## Round-1 dispositions

| Finding | Disposition | Closure evidence / remaining work |
|---|---|---|
| **B1** out-of-band gate never completes an already-reached flush | **CLOSED** | §4.4.1 rule 1 case A commits and acks immediately; the queued copy becomes a no-op. The exact directed witness finishes under rev1. |
| **M1** no completion path under the hard ceiling | **PARTIAL** | DEC-80 Q4's oversized-op policy is a valid solution in principle; waiting/freezing at op boundaries and drop-before-`FlushDone` fix the original control-flow and release race. The claimed reservation bound is not valid for fetched/restored input coins or all growth transients: R2-M1 below. |
| **M2** engine-memory bound unproven | **CLOSED** | DEC-80 Q3 explicitly accepts E as a configured target. §2.5 withdraws the hard bound, specifies non-strict LRU, `allow_stall=true`, disables L0 pinning, and adds native memory/RSS diagnostics; DEC-43's `rss_anon` gate under the shared cgroup regime is binding. No claim that these engine options mathematically enforce E is needed. |
| **M3** no locator-target state contract | **CLOSED** | §2.9 journals rows and state on `(L,p]`; fixed state keys occur only in locator batches, using `state_at(X)`, and recovery loads that state before blind deletes. `reverse`, `DurableBelow`, archives, `commit_state` and resource barriers cover rewinds and monotone filter-file positions. This is the missing coinstats-shaped contract, not merely a height-row toy. |
| **M4** legacy undo can select the wrong branch | **CLOSED** | §3.5.2 keys discovery by spending block and txid, filters occurrences by that block's ancestry and in-block order, and explicitly refuses/clears unresolvable active/stale records. The two-fork fixture directly covers the round-1 example. |
| **m1** hidden-freeze cache membership | **CLOSED** | §2.4.1 defines first-hit unspent membership and active-only `uncache`; D12 declares the justified difference. §6.2 aligns membership comparisons instead of demanding impossible hidden-freeze equivalence. |
| **m2** ancestry before index/tip load | **CLOSED** | §4.3 loads file metadata and index, selects G, then registers ancestry-derived locks before pruning or activation. Wipe resumption precedes marker checks. |
| **m3** VARINT numeric ordering | **CLOSED** | §2.2 describes Core's serialized-key ordering; §6.1 includes the 16383/16512 boundary vector. |

The other notes in my round-1 report are also carried: signed/clamped prune arithmetic, protocol-barrier counting, WAL trigger wording, qualified density claims, different-branch benchmarks and prefetch/F-owned-key regressions. DEC-78's corrupt-coin abort is explicitly declared and covered by the corruption test, X21 and FA7.

## Exact B1 rerun

The supplied `gpt_b1_state` really starts at **L=0, p=8, D=8, F.new=8**, with **Connected(9)…Connected(17)** already queued before B1. It is not a smaller analogous schedule.

After B1, my direct transition check gives:

- **rev0:** `S0.ckpt@(8,0)`, L=8, ack set empty. Its explored graph has **23 states, zero done states, 23 unable to finish**. The later checkpoint at p=16 cannot advance L or consume the outstanding queued commit.
- **rev1:** `S0.commitA@(8,0)`, L=8, ack set `{0}` immediately. The explored graph has **104 states, two done states, zero deadlocks and zero states unable to finish**.

After ack, W can finish/release. Processing later blocks may park at 17 and request the next flush, which is now possible. This is exactly the missing edge identified in round 1. A fork below p changes the commit target to the allowed LCA; it does not reintroduce “checkpoint repeatedly, never ack.” The supplied branch grid also exercises two subscribers and the shared events thread.

Evidence: `probes.py`, `probes.txt`, `model-results.txt` in the round-2 scratch directory.

## The model and rev1's three additional holes

I re-ran the **full** supplied model, without `--quick`: **90 configurations, 1,777,714 grid states, ALL PASS**. The directed B1 and F9 checks pass, and removing each of X17–X20's protections produces a failure. My independent audit also exercises the late-Synced missing-target case.

| Hole | Assessment |
|---|---|
| **X19**, backward B1 | **CLOSED at design level.** Step 3b tests every mandatory durable L, including stale-branch L, rather than just old versus new. The lowered cap prevents new forward commits above new while the gate is open. Synced targets consume already-issued disconnects; Syncing targets rewind on their own thread. B1 waits until no L strictly descends from new. The no-gate plant fails R11a. DEC-80 Q7 authorizes this pre-B1 rewind wait. |
| **X20**, subscriber parks on an already-queued canceled connect | **CLOSED for the reported schedule.** Compression happens before parking; removing it reproduces the stranded subscriber. A flush wakes a parked subscriber, and after its case-A ack the canceled connect is compressed rather than parked on again. The fix needs the actual dispatcher/queue implementation to preserve that order. |
| Subscriber became Synced after new was disconnected | **CLOSED.** Case B requires the target connection to be ahead in this subscriber's queue, rather than inferring it from height or demanding that it was previously processed. With p=B4, L=fork2 and outstanding new=A3 never delivered, the model takes `commitA@fork2` and acks; it does not discard B's branch while looking for an absent A3 event. |

The model faithfully captures the central B1 fix, out-of-band commits, nonblocking parking, capture-time Synced classification and the rewind-gate dependency. It intentionally does not prove subscriber row/state correctness, allocator bounds, I/O, pruning or shutdown. Those remain production-seam acceptance obligations.

### R2-m1 — MINOR: the modeled queue bound is not actually enforced for reorgs

In `v_moves`, `room` checks current queue length `< Q` once, then `V.reorg` appends **all** depth-REORG disconnects atomically. For **Q=2**, the independently explored `(SS,D=1,Q=2,ACT=3,REORG=2)` graph reaches a **three-data-event queue**. `invariants` does not check this bound. See `model-audit.py` / `model-audit.txt`, including the transition trace.

Real per-event backpressure can stop V between disconnects; the model skips those states and intermediate tips/freezes. Therefore “every interleaving of bounded queues” is stronger than the model checks. Model disconnect emission one event at a time with intermediate V tips, and assert data queue length ≤ Q. Alternatively explicitly define/reserve an atomic reorg-batch allowance in both CH1's contract and the model. Re-run the grid before porting it to Rust.

Two further limits should be made explicit:

- **AG EF done** proves that every modeled state has some completion path. It does not rule out every infinite execution that repeatedly ignores enabled progress. State fairness/finite-I/O assumptions; avoid saying it detects all livelocks. It correctly detects my rev0 fixed point because no completion path exists there.
- The model uses a count of V changes (`since`) as the active/freezable abstraction. Lemma 2 must also cover coin-empty changes, a round trip back to the last frozen tip, and a disconnect arriving after a subscriber has parked. A scratch probe of an already-parked subscriber with a later matching disconnect and no freezable work has no transitions, because the parked early return precedes compression. This injected state is **not a demonstrated reachable counterexample to the supplied grid**. Specify either the invariant that always supplies a subsequent flush/wake, or a queue-change wake that rechecks compression; cover it in the threaded test. Do not infer a new blocker from that injected state alone.

## New major

### R2-M1 — MAJOR: rule O's `T(op)` and `T_max` do not bound long input coins

**Where:** §2.5, especially lines 493–510; the consumers in §2.4.2; §6.7's asserted bound; DEC-80 Q4's quantitative claim.

`long_bytes` is said to be at most the operation's serialized size. That is false for a connect's fetched inputs and a disconnect's restored inputs. The spending transaction serializes **outpoints and scriptSig**, not the historical output scripts. Undo carries the historical coins and can be much larger than the block it undoes.

A concrete consensus-shaped witness uses a 9,577-byte script:

`(PUSHDATA2 500 zero bytes; OP_DROP) × 19; OP_TRUE`

It is below the 10,000-byte script limit, each push is below 520 bytes, it has only 19 counted non-push operations, no sigops, and leaves a single true stack item. It is neither OP_RETURN nor oversized, so the historical coin is stored. The script is not one of the compressor's special forms. These outputs can be created across earlier blocks and spent with empty scriptSigs; nonstandard relay policy does not exclude consensus-valid blocks.

**Two transactions of 24,000 inputs each** stay below the per-transaction 1-MB legacy limit (`consensus/tx_check.cpp:32` at v24). Together, **48,000 inputs × 41 bytes + 1,000 bytes overhead = 1,969,000 bytes**, below the stated 2-MB block ceiling. Yet even ignoring coin headers, amount/length bytes and all allocation overhead, one copy of their bytes beyond the inline allowance needs:

`48,000 × (9,577 − 34) = 458,064,000 bytes ≈ 436.84 MiB`.

That exceeds **both** the stated 47.7-MiB arena and 90.8-MiB fallback `T_max`, before child/parent overlap. `have_inputs` expressly caches every input into tip and child; a disconnect restores these coins. At tiny B this defeats the promised overshoot cap; at ordinary B the declared reservation can classify an operation as fitting while its actual allocations exceed B. Waiting or freezing first cannot remove the operation's own historical-coin cost.

There is a separate accounting issue in the same proof: arena `GROW` is described as a 262,144-byte slot chunk, but its `HashTable<u32>` can rehash an existing large index. One insertion can require a new index allocation proportional to the current table, not one slot chunk. The reservation must count the actual next allocation and old/new overlap; the current constant does not establish that. Likewise specify the aggregate bound across fallback shard growth and a concrete `scratch_cap`, rather than treating their names as a proof.

**Required correction:** retain approved rule O, but derive T from the coins that can be fetched/restored (or a conservative per-input script bound), serialized outputs, exact allocation/growth overlap and bounded scratch. For disconnects, account for undo bytes. Derive a valid worst-case constant or use a bounded streaming/spill strategy with a proof that preserves completion and the accepted memory policy. If the corrected maximum changes DEC-80 Q4's numeric promise materially, report that to the manager. Add long-script high-input connect/disconnect and near-capacity index-rehash cases to allocator acceptance, alongside the existing high-output test.

Evidence: `memory_witness.py` / `memory-witness.txt`; pinned v24 `script.h` constants and `IsUnspendable`, `interpreter.cpp` size/opcode checks and OP_DROP handling extracted into scratch. This is an arithmetic/consensus-shaped design witness, **not** a claimed execution of unimplemented CH2 code.

## Legacy undo and rename rewrite

The branch-resolution rule closes the original A100/B110 ambiguity. It selects occurrences on S's actual ancestry, including earlier in-block transactions, and refuses ambiguous/unavailable metadata rather than inventing it. Clearing stale-branch undo positions forces fresh undo when those blocks are reconnected. Recovery remains blind and rejects zero-height undo.

The rewrite also closes the source-inode hazard: temp file in rdashd's directory → normalize and recompute checksums/positions → profile finalization and file sync → rename over the destination hard link → directory sync → index batch. Rename replaces the destination directory entry, leaving the source link's inode unchanged. Updating **every** position in the rewritten file is necessary because record lengths change, and rev1 says so. This reviews the approved mechanism; CH6a must test interrupted import/resume and source hashes as implementation acceptance, not silently fall back to ST2 append into a linked inode.

## Handoff

No new blocker found. **One new major (R2-M1)** prevents closure of M1 and design approval. Fix the quantitative memory proof while retaining rule O, tighten the model's queue semantics, and rerun the affected closure checks. B1, the locator-target state contract, configured E and ancestry-aware/source-safe normalization need no policy redesign.
