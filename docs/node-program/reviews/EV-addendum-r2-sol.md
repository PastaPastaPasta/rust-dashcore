# EV addendum to SB0 — design review, round 2 (Sol)

| | |
|---|---|
| Under review | `docs/node-program/designs/EV-addendum-SB0.md` and `docs/node-program/tools/cpm_ev_addendum.py`, rev1 at **37cff995f5adafec00d9bfb7106071cbbeb39de2** |
| Reviewer | Sol, sequential first pass under DEC-92 |
| Contract | `briefs/REVIEW-COMMON.md`; PLAN §7 |
| Inputs | Sol r1; DEC-104/105/107/109/111; approved EV2/LQ1 contracts; EV0 **3a5cb631**; the pinned CPM inputs |
| Core | Exact tag reads at **v24.0.0-rc.3** and **v23.1.8** |
| Date | 2026-10-09 |
| Result | **APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** One major remains at the attempt's concurrency boundary. |

## Evidence and limits

The reviewed tree was a detached checkout under `/work/scratch/ev-add-r2-sol/wt/`. A second detached checkout of EV0 supplied the existing entry and BLS types for boundary probes. Test temporary files used `/dev/shm/ev-add-r2-sol-tmp`, mode 700. No reviewed branch was modified, committed, pushed, or commented on GitHub.

- `python3 docs/node-program/tools/cpm_ev_addendum.py`: all six runs and both reported chains reproduced. Assertions checked all **54 finish values**, the EV0/LQ1a/LQ2 edges, and a copied-input negative control: modifying the pinned `cpm.py` makes the tool refuse to run.
- EV0 baseline, `cargo test -p dash-evo --all-features`: **32 passed** (18 library, 8 consumers, 6 store).
- Two-crate `rustc` probe: the shared `Arc<T>` forwarding implementation and external, local `Dmn` implementation compiled; **1 test passed**, exercising all six entry methods through the shared generic consumer.
- C++ probes compiled with `-Wall -Wextra -Werror` against each tag's actual `unordered_lru_cache.h`: a separate const lookup leaves values, access timestamps/counter and the base cache unchanged, including an indexed-only hit. Normal `get` plus base insertion does mutate them. Subsequent eviction confirms that observing an entry did not protect it from eviction.
- Scratch-only EV0 observer tests: **3 passed**. Cloning `Arc<Dmn>` for a peek does not decode its key. Cloning the lazy key wrapper before decoding/serialization preserves production state. The negative control confirms that a shallow `Dmn` clone still shares `Arc<MnCold>` and does not isolate decoding. The baseline also passes the later-`core_eq` and serialization isolation tests cited by rev1.
- An illustrative attempt-boundary model passes the five specified conformance cases, including the original per-call-overlay negative control and failed/check-only persistence. A separate schedule enumeration reproduces the concurrent visibility defect below: the addendum permits `(new-roster, old-row)`; no Core schedule preserving the store-before-cache-insert and reader's hit-before-row-read orders permits that pair.

These are design and boundary checks, not execution of the proposed CH7b implementation or the future LQ1a/LQ1b/SB19 acceptance suites. Those implementations do not exist at this revision. All reviewer-created checkouts, probes, build outputs and temporary files were removed after review. This review is uncommitted.

## Required change

### R2-M1 — Delayed rows and immediately published members expose a state Core cannot expose

**Severity: major. Location:** `docs/node-program/designs/EV-addendum-SB0.md:256`; also `:252`, `:253`, `:254`, `:255`.

The attempt-owned overlay fixes same-attempt visibility, but the claim that arbitrary concurrent readers can simply be placed “before the write” is too broad. The attempt delays a snapshot overwrite until its log is applied while immediately publishing the members computation to the process-wide caches. A reader can therefore observe the computation's completion and still read its old snapshot through the runtime backing rows.

**Concrete failure scenario:** a connected, older cycle K has snapshot S0, and its members-cache entry has been evicted. An attempt recomputes K with changed inputs, producing snapshot S1 and roster M1. Such overwrites are expressly allowed by LQ1 §3.7.1; pruning or changed previous rows can change the inputs. The handle records S1 in its private overlay and log, and the facade publishes M1. Pause the attempt before log application. A non-attempt reader first obtains M1 from the process-wide cache and then reads K's snapshot through the runtime row reader, for example while computing a later cycle. It receives S0. A later computation can retain a roster derived from that stale row, even after S1 is applied and even if the original attempt fails or is a proposal.

This cannot be justified as a Core interleaving. In v24, the rotated computation calls `StoreSnapshotForBlock` at `llmq/utils.cpp:462`; `snapshot.cpp:326-328` locks and overwrites the raw row and snapshot cache before returning. Only after that computation returns does `GetAllQuorumMembers` publish the indexed entries (`utils.cpp:710-713`) and base entry (`:727-728`). v23 has the same order (`utils.cpp:453`, `snapshot.cpp:289-291`, `utils.cpp:639-642`, `:656-657`). If the reader observes the newly inserted M1, Core's S1 write already happened. Its subsequent snapshot read cannot obtain S0 without another overwrite, which this scenario excludes. The locks impose an order; absence of a single global lock does not remove it.

The scratch schedule enumeration preserves both threads' operation order and exhausts the possible store/insert/hit/read interleavings. Every successful hit of M1 is followed by a read of S1. The proposed contract instead admits M1 followed by S0. The five sequential `ModelAttempt` cases do not exercise this boundary.

**Suggested fix:** retain DEC-111's single overlay and ordered log, and make the serialization boundary explicit. The simplest option is to restore/enforce approved LQ1 §3.7.2's serialization with validation for all external members, materializer, predictor and snapshot-read routes: they must not consume the facade's in-progress state until CH7a has applied the log, on every outcome, and released the attempt. Give mnxcheck the same coherent read boundary, so its miss path cannot combine published members state with unapplied rows. Alternatively, make pending snapshot overwrites visible to every such reader before publishing the corresponding cache entries, with explicit ordering against runtime overwrites. Merely locking the handle's private overlay is insufficient.

Add a conformance case that pauses after a rotated cache insertion and before effect application, starts an external cache-hit → snapshot-read/later-cycle call, and proves it either waits or sees S1. Run it for successful, failed and proposal attempts against the model and CH7b; cover the indexed-cache hit as well as the base hit. If external writes remain concurrent, also test that a later runtime overwrite is not reversed by replaying an earlier pending effect.

This does **not** reopen DEC-111's overlay decision. It closes an additional correctness condition needed to make that decision compatible with Core's process-wide caches. The ownership, same-attempt visibility, unconditional overwrite and failure/proposal persistence choices remain sound.

## Disposition of round 1

| Finding | Round 2 result |
|---|---|
| **M1** orphan rules | **Closed.** §1.3 and §3 commits 1/3 put the forwarding impl in the trait-owning shared crate and the concrete impl in `dash-dml`. The cross-crate probe compiled and exercised the forwarding methods. `Entry = Arc<Dmn>` stays cheap to clone. |
| **M2** ancestry fallibility | **Closed.** §2.2 defines `Ok(None)` precisely and keeps known before-genesis termination distinct from unavailable ancestry. §2.5 covers the predicates, enabled-type vector, routing, rotation and materialization seams. Replacing `precompute_targets` with Core's loop preserves the eager enabled-type pass followed by interleaved rotation checks and members calls. The loop matches both tags; the required negative controls are named. |
| **M3** cached membership | **Closed as a comparison choice**, subject to R2-M1's coherent boundary. Peek on a hit, isolated recomputation on a miss and `NotComparable(PrunedWorkBlock)` for a miss that actually reads `BlockDataUnavailable` correctly distinguish the facade's answer from a fresh answer. A hit must remain comparable despite subsequent pruning. |
| **M4** CPM mismatch | **Closed.** The normative EV0 row and executable model both retain `CS0, i:BS6a`; the old merge-edge interpretation is now explicitly X, with REL11 **31.5**, and X1 gives **32.0**. P/P1 give **30.0**; P2/P3 give **31.5**. The pins and input-change rejection work. |
| **M5** unspecified attempt contract | **The original contract gap is resolved**, but full Core parity still needs **R2-M1**. CH7b owns one attempt-tagged overlay shared across calls and precompute, effects stay ordered and apply on every outcome, and process caches survive failure/proposal attempts. Cases 1–5 cover the original sequential defects. |
| **m1** final normative docs | **Closed.** EV0 r3 now reviews the executable delta with move detection and the final `API.md`/`ARCHITECTURE.md`, including m2's resolution. |
| **m2** observer isolation | **Closed.** §4.3 requires overlay-first rows, direct backing reads without snapshot-LRU mutation, private/no memo, cloned lazy fields and cloned observer digests. The key-wrapper tests substantiate the isolation mechanism and its shallow-clone negative control. |
| **m3** rotated acceptance phase | **Closed.** SB8 accepts verifier-only and non-rotated fixtures; SB11, already behind LQ1b, owns rotated and wrong-row composition tests. No unimplemented rotated route is accepted as the final test. |

## Specific checks

**Can peek avoid the Core-visible observer effects? Yes.** It must be a distinct lookup primitive, not `get`, `exists`, or a normal facade call followed by an attempted rollback. Both Core LRU methods `get` and `exists` bump access state; an indexed hit through the normal facade also copies into the base cache. The new contract explicitly excludes all three mutations. Copying an `Arc` roster itself does not decode the operator keys. Every subsequent projection, serialization or comparison must clone each lazy key wrapper as §4.3 requires; cloning only the entry/list is insufficient. Direct snapshot-row reads and private/no memo complete the specified cache isolation. This is implementable, with the implementation tests still due in CH7b/SB19. It also needs the coherent boundary identified in R2-M1.

**Does the attempt preserve failure/proposal semantics? Sequentially, yes.** The log is applied despite failure, `fJustCheck`, proposal and VerifyDB outcomes; runtime writes are synchronous unconditional overwrites; cache inserts are process state; the next attempt opens its backing view after application. Two calls and multiple overwrites now have the correct row visibility. No additional LQ1a start gate is needed for the formerly undecided ownership contract. The remaining major concerns concurrent consumers of that state, not ownership of the overlay or permission to code LQ1a against it.

**Is `i:BS6a` legitimate when BS6a has merged? Yes, in the stated historical model.** The model deliberately keeps merged rows as scheduled rows, and `i:` represents the historical coding overlap against an available interface. Keeping that edge does not pretend the already-satisfied real-world gate will delay this reshape. The actual reshape rebases onto the merged pin; the wrapper relocation follows EV0 under DEC-109. §5 now separates these facts and discloses the alternative numbers and size sensitivities. The 30.0 result is a result of that pinned week-zero graph, not a progress-aware estimate of remaining calendar work; the document expressly says no remaining-work model was built. There is no numerical or edge mismatch left in the addendum/tool pair.

DEC-104/105/107/109 remain accepted. DEC-111's chosen overlay remains accepted with the required visibility boundary. No additional blocker, major, or minor finding is raised.

## Verdict

**APPROVE-WITH-CHANGES. Sol gate NOT satisfied.** Resolve **R2-M1** before starting the sequential Opus-high pass, unless the manager explicitly uses DEC-92's escalation alternative. The split, fallible interfaces, observer strategy and corrected historical CPM can stand.
