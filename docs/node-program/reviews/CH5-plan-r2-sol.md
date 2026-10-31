# CH5 rev1 narrow plan re-review — Sol r2

(Durable copy of /work/scratch/ch5/reviews/plan-r2-sol.md; wording changed only where the docs commit gate requires it.)

Compared `PLAN-rev0.md` with `PLAN.md` and checked the r1 findings, Q1–Q5 and acceptance audit against the changed plan. Core references below are **v24.0.0-rc.5**, read with `git show`; rdc references are read-only from `/work/home/agent/workspace/rdc-wt/integration`. Locations without a repository prefix refer to `/work/scratch/ch5/PLAN.md`.

R1/R2 do **not** require rev1 to reinstate a Core-model meter. DEC-99, DEC-102 and CH2 D11/D12 make manager escalation the appropriate route. This review judges the accuracy and completeness of that escalation, not whether the manager should accept its proposed difference. A pending manager decision alone is not counted as an unresolved review finding.

## Per-finding disposition

| Finding | Status | Rev1 section and reason |
|---|---|---|
| R1 — major | **PARTIAL** | **§3 E4-1**, lines 323–328, correctly identifies retained pool allocation/history, differing flush epochs and DEC-102's removal of the meter. **CH5-1a acceptance**, lines 89–92, adds an equality seam and makes boundary acceptance depend on the ruling. However, E4-1 understates the observable consequences and promises unproved equality away from a “boundary” and a mainnet skip. It also calls scratch + tip-active accounting the same Σ as the flush policy, although that policy includes other components. See the residual R1 discussion below. |
| R2 — major | **PARTIAL** | **CH5-1a scope 1**, lines 59–62, acknowledges clean parent fills and preserves authoritative values; **§3** mentions their memory contribution. But it incorrectly treats omitted fills as already sanctioned by D12 and says the L3 figure is their only other observable. Missing fills can change ATMP reasons and P2P orphan/parent-request behavior even with **no intervening flush or release**. That is outside D12's stated flush-history exception. See residual R2 below. |
| R3 — major | **CLOSED** | **CH5-1a scope 3/7**, lines 68–78, preserves Core's conditional undo/index writes and places failure-flag reset only at startup. **CH5-1b effects and acceptance**, lines 111–131, covers peer disconnects, list/quorum/MNHF/credit-pool caches, platform-ban metadata, CL/IS, script-cache/IBD and BLS; compares Core's observed transition; includes L3, skip, failure and interruption. Unavailable production effects are explicitly gated/deferred rather than inferred from unchanged healthy RPC answers. This is closure of the plan finding, not certification of future effects code. |
| R4 — major | **CLOSED** | **CH5-1a acceptance**, lines 85–88, moves output-value corruption to L3 and specifies a checksum-correct, pre-proved L4-only undo fixture. **CH5-2 scope 3**, lines 149–152, requires a deliberately malformed, solved pre-BIP34 coinbase, old/new activation comparisons, full-reindex rejection and a separate header-context fixture/mutant. Both fixture sets must be proved on Core before implementation starts. |
| R5 — major | **CLOSED** | **CH5-3 scope 1**, lines 190–200, separates explicit fresh wipe, interrupted wipe and stored-flag resumption; resumption retains index/file info/cursors and skips already-known data. Fresh wipe resets `meta.last_file` and other file cursors. **Acceptance**, lines 219–226 and 237–243, checks retained records/positions and the combined-flag sites. The newly specified handoff between markers has a distinct new bug, N1 below. |
| R6 — major | **CLOSED** | **CH5-3 scope 3**, line 208, replaces EOF append with rebuilt logical undo size zero and Core finalization/truncation, while retaining resume cursors. **Acceptance**, lines 227–232, covers nonempty rev files, repeated fresh reindex, fork/out-of-order history, multiple blk files, byte equality under controlled starting conditions, logical offsets and checksums. |
| R7 — major | **CLOSED** | **CH5-3 Core/scope**, lines 176–183 and 201–207, gives scan → clear → genesis retry → external import → final ABC, distinguishes Core's non-sync marker calls from proposed stronger wipe durability, and **acceptance**, lines 219–225 and 235–236, adds post-clear/pre-ABC and mid-ABC crashes and fixes the mutant to early clear during the scan. N1 concerns the separate fresh-start handoff, not this corrected end-of-scan ordering. |
| R8 — major | **CLOSED** | **CH5-4 scope 2**, lines 258–263, supplies an external journal, backup identity/original directory set/phases, persisted intent, parent-directory fsyncs, idempotent recovery and continuous datadir locking; routes both early refusal hooks. **Acceptance**, lines 267–276, covers rename boundaries, the final-rename gap, existing backups, lock contention and power-loss/fsync mutants. The downstream reindex marker handoff must also be fixed under N1. |
| R9 — major | **PARTIAL** | **CH5-5**, lines 278–298, now names WBM, WAL, `after_bk`, ordinary flush and compaction paths and requires a deterministic copy/recovery/continuation test. But option 1 does not explain how per-CF memtable-size flushes, the fallback DB-wide cap, WAL rotation/reclamation, or another explicit copy point are prevented or made safe. Option 2 does not specify how an unmodified raw datadir copier sees the pinned checkpoint rather than the live DB. Reviewing a concrete choice before code is a useful gate, but neither option yet establishes the promised copy contract. See residual R9 below. |
| R10 — major | **CLOSED** | **§1 order and CH5-1b**, lines 31–42 and 111–131, separate non-evo/refusal acceptance from positive evo/effects, gate the latter on CH7a-5/CH7b-2 and production CL/IS hooks, retain BL-524/D46 until positive coverage, and forbid bypassing NotReady. **CH5-2 acceptance**, line 163, defers evo state hash coverage. **CH5-6b / §5**, lines 302 and 357, gates the second segment on CH5-3, P3 and the required evo capabilities. |
| r11 — minor | **CLOSED** | **CH5-4 title/acceptance**, lines 245 and 268–269, correctly tests dashd source → rdashd destination with both actual framework copy flows. **CH5-5** separately owns rdashd → rdashd. |
| r12 — minor | **CLOSED** | **CH5-1a**, lines 49–50, 67–68 and 83, names the UNCLEAN early return and predecessor-based reconnect and tests depth 1/full depth. **CH5-3**, lines 187 and 207, separates unconditional import/count lines from REINDEX diagnostics; **CH5-1a acceptance** requires debug off/on where logs are compared. |

## Remaining parts of prior majors

### R1 — major — §3 E4-1: describe the actual scope of the proposed difference

**Location:** lines 63–77, 89–92 and 323–328.

The escalation accurately explains why an exact Core figure is not already available under the program rulings. Its proposed observable scope is incomplete. A different gate changes which blocks reach L3, whether L4 runs, the RPC boolean and diagnostic/progress path, and startup's result: explicit verification can fail with insufficient dbcache while default verification continues. On corrupted data, a mismatch in the disconnected prefix can instead change whether an inconsistency is detected and whether startup refuses. Once evo effects are enabled, it can also change the prefix of retained disconnect/reconnect effects. These consequences follow Core's `validation.cpp:5119–5177` and `node/chainstate.cpp:441–454`; they are not solely a memory-report difference.

There is no stated bound relating native and Core usage, so “results equal Core's away from the boundary” does not establish a common verification range. Likewise, height 1,028,159 alone does not prove that both nodes skip under the eventual options/cache histories. Keep that M3 equality comparison as measured acceptance, not an assumed result.

The recommended figure also needs an exact definition for the manager to approve. `Usage::total()` includes active, frozen, prefetch, overlay, held-out undo, reserve and unbacked capacity (`dashd-chainstate/src/coins/budget.rs:94–123`); “scratch + tip active layer” is a projection of those charges, not the same total used by CRITICAL/LARGE. Choosing that projection is possible, but its exclusions and scratch allocation accounting must be explicit.

**Fix:** retain E4-1, define the proposed figure precisely, enumerate the return/startup/log/coverage/effect consequences in its recommended allowed-diff text, and require observed M3 equality under named options. The manager decides whether that scope is acceptable; this review does not select a counter or demand restoration of the removed meter.

### R2 — major — CH5-1a scope 1: omitted read fills extend beyond the existing D12 exception

**Location:** lines 59–62 and 323–326.

**Failure scenario:** flush first so a mined non-coinbase transaction T's unspent output is in the DB but absent from both tip caches. Run `verifychain 3` over T, then resubmit T without a flush/release between those two operations. Core's scratch `SpendCoin`/`FetchCoin` causes the RPC parent to retain a clean copy of T's output (`coins.cpp:44–65, 129–149`). T's inputs are spent in the authoritative tip. Core can therefore classify T as `txn-already-known`, while an implementation omitting the parent fill classifies it as `bad-txns-inputs-missingorspent` (`validation.cpp:972–990`). On P2P that classification controls the orphan path and parent requests, as documented in CH2 §2.4.5. Uncache bookkeeping and subsequent cache/flush behavior can differ too. Coin values remain unchanged.

DEC-102 and CH2 D12 allow membership differences **across differing flush/release histories**, and expressly claim equality in the no-intervening-flush case. Omitting a caching operation is an additional cause of membership divergence; the existing ruling does not automatically approve it. This consequence is separate from the L3 accounting choice.

**Fix:** either preserve the RPC parent's clean fills through a native cache API, or explicitly add this no-flush membership/reject/orphan difference to the manager escalation and proposed allowed-diff scope. Add a cold-parent DB-hit + repeated-verification case and a post-verification membership/ATMP case without an intervening flushing RPC. Clean fills do not require reinstating a Core memory shadow or changing authoritative coin values.

### R9 — major — CH5-5: complete one copy protocol before approving that slice

**Location:** lines 278–298.

**Failure scenario:** disable WBM, the explicit `after_bk` flush and auto-compaction, but continue writing until a CF's finite memtable fills. RocksDB can still create an SST and update the MANIFEST while the raw copier is between directory enumeration and MANIFEST copying. Removing WBM can also leave the fallback DB-wide cap enabled; setting the WAL limit to zero is not a way to disable its size trigger. Those meanings are explicit in `dashd-store/src/rocks.rs:27–35, 53–63, 216–223, 685–692`. A second explicit `gettxoutsetinfo`/stop flush during an unacknowledged copy interval can recreate the same race. WAL file rotation/reclamation also needs treatment. A checkpoint elsewhere does not protect `shutil.copytree(source_data_dir, ...)`, which copies the live tree (`test_framework.py:627–636`).

**Fix:** make the pre-code slice-plan gate require a concrete, reviewed copy-point/interval contract and an exhaustive trigger/file-lifetime mechanism, including size-driven memtable flushes and concurrent explicit copy points. If the solution uses a checkpoint, specify how both framework copy flows actually read that generation and how its files remain pinned. Pin this with deterministic threshold-crossing and second-copy-point interleavings plus the already-required node recovery/state hash/continuation checks. Until that design is accepted, CH5-5 remains gated; the listed tuning changes alone are insufficient.

## New findings introduced by rev1

### N1 — major — CH5-3 fresh-start protocol clears the only intent before establishing resumption

**Location:** CH5-3 scope 1, lines 190–198; CH5-4 handoff, lines 259–261.

**Failure scenario:** begin a fresh `-reindex` on a normal store whose `reindexing` flag is false. Complete the wipe, durably clear the wipe intent (line 194), then crash before `reindexing = true` (line 195) is durable. Restart without CLI flags. There is neither an interrupted-wipe intent nor a stored reindex flag, so none of the specified restart cases requests the blk scan. The node can initialize genesis beside the old blk files instead of rebuilding the erased index/chainstate. This is a process-crash gap even if every individual marker write is durable. The existing `wipe_chainstate` really does clear its marker in a separate durable batch (`dashd-chainstate/src/store/meta.rs:210–243`), so merely extending its erased CF set is insufficient. Auto-import inherits this gap when it hands off to CH5-3's fresh-reindex protocol.

**Fix:** establish a durable reindex/resume state before removing the durable wipe intent, or perform the transition atomically in one durable batch. If the markers live in different files/stores, specify the write/fsync dependency and startup precedence. No accepted scan work should be exposed to a still-replayable wipe phase. Add kill/model cases immediately after wipe completion, after each marker transition and before the first scanned record, including the auto-import handoff. Every such restart must request wipe recovery or scan resumption, never ordinary empty-store initialization.

### N2 — minor — CH5-4 states the serialized key prefix length incorrectly

**Location:** line 254.

**Failure scenario:** a detector following “its 14-byte length prefix” will not find Core's actual LevelDB user key and will refuse a valid cache/source. Core's string payload is 14 bytes (`dbwrapper.cpp:228–234`); string serialization prepends a **one-byte CompactSize value `0x0e`** (`serialize.h:1038–1043`). The serialized user key is 15 bytes total: `0e 00 6f 62 66 75 73 63 61 74 65 5f 6b 65 79`.

**Fix:** specify that exact serialized key and add a literal-key detection fixture, including its WAL-only version. The live-table/WAL sequence and tombstone requirements in lines 255–257 otherwise address Q4 correctly.

## Acceptance audit and limits

The prior audit's non-code completion issues are addressed: CH5-2 lines 139, 153–159 and 164–172 preserve partial-batch-only crash rolls in production and recovery, ratio-1/no-partial coverage and repeated recovery/reorg crashes; lines 165 and 316–317 keep the unadapted FTs pending on IX1/MP1a/b and reject owner reassignment as exit-5 proof. CH5-3 lines 214 and 312 retain BL-507 as a pending ST3b device/VM acceptance gate and require the separate ST3a model case. Q3's controlled byte/offset comparison and Q5's CH5-3-before-CH5-4 dependency are preserved.

This was a source/plan review. No implementation was changed or built, and no future process-test result is claimed. No commits, pushes, GitHub actions, managed datadir/cache/benchmark access or restricted-tree access occurred. Only this requested scratch review file was written.

## Verdict

**REWORK** — R1, R2 and R9 remain partial majors; N1 is a new major. No new blocker was found. The counter/allowed-diff decision remains the manager's.
