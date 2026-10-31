(Durable copy of /work/scratch/ch5/reviews/plan-r3-sol.md; wording changed only where the docs commit gate requires it.)

| Item | Disposition | Rev2 section and evidence |
|---|---|---|
| R1 — major | **PARTIAL** | **§3 E4-1, PLAN.md:370–390** now defines `Usage::total()` with all seven components, charges scratch as an overlay, specifies the comparison, enumerates coverage/return/startup/detection/effect differences, and makes M3 equality measured acceptance. The counter remains the manager's choice. However, **:368** excludes membership from the escalation, although the different L3 prefix can itself produce different parent fills and hence membership/reject/orphan behavior. **:383** also places cache-effect differences behind CH5-1b, whereas these coins-cache effects exist in CH5-1a. See the remaining R1 finding below. |
| R2 — major | **CLOSED** | **CH5-1a scope 1, PLAN.md:59–63**, requires every scratch miss on the RPC path to use the tip's fetching read; **acceptance :110–118** adds the dropped-fill mutant, cold DB hit/repeated verification and post-verification rejection/membership cases. This is feasible with the native cache; it does not require a Core-model memory meter. For the same executed scratch-read sequence and corresponding initial membership, it preserves Core's parent-fill membership. Different verification ranges remain an E4-1 consequence under R1, not an independent omitted-fill problem. Implementation requirements are explained below. |
| R9 — major | **CLOSED** | **CH5-5, PLAN.md:306–329** explicitly forbids code before a reviewed, closed copy-contract note. The required note must establish the interval and bound B, account for per-CF and aggregate memory triggers, WAL semantics/rotation/reclamation, ordinary and explicit flushes, compaction and flatfile lifetimes, and resolve concurrent copy points. A checkpoint choice must explain how both actual live-tree copy flows see the pinned generation. Deterministic threshold/flush/second-copy interleavings must recover through the node and check blocks, coins/evo, state hash and continuation. This is sound plan-level closure; the unwritten note and either implementation option are not approved by this review. |
| N1 — major | **CLOSED** | **CH5-3 scope 1, PLAN.md:197–206**, establishes `{wipe_intent=1, reindexing=1}` in one durable batch before wiping, then durably clears only the wipe intent, with startup precedence specified. **:227–234, :247** add the transition crash cases and mutant. **CH5-4 :268–276, :293** keeps journal completion durable before the first scanned record, so replaying an incomplete handoff cannot erase accepted scan work. The former state with neither durable intent nor durable resume flag is removed. |
| N2 — minor | **CLOSED** | **CH5-4 scope 1, PLAN.md:264–265**, specifies the exact 15-byte serialized user key, its one-byte CompactSize `0x0e`, and literal table/WAL-only fixtures. This agrees with Core `dbwrapper.cpp:228–234` and `serialize.h:1038–1043` at v24.0.0-rc.5. |

## Remaining R1 — major — different L3 coverage also changes parent membership

**Location:** `/work/scratch/ch5/PLAN.md:368`, also :61, :376–387.

**Failure scenario:** start with a mined non-coinbase transaction T whose unspent output exists in the DB but is absent from both tip caches. Let Core's gate permit disconnecting T's block while the native gate skips it. The proposed figures have no proved relation, and the escalation explicitly permits a different disconnected prefix. Core's disconnect spends/checks T's output through the scratch view (`validation.cpp:2342–2349`). The scratch `FetchCoin` calls the RPC parent's fetching `GetCoin`, retaining T's unspent output in that parent (`coins.cpp:44–65, 129–149`). The node that skips this block performs no corresponding read and retains no such entry.

With no flushing or release operation between verification and resubmission, T's inputs are spent on both nodes. Its cached output makes Core return `txn-already-known`, while the node without the fill can return `bad-txns-inputs-missingorspent` (`validation.cpp:972–990`). The P2P missing-input branch can then request parents and retain an orphan (`net_processing.cpp:4976–5010`). The discrepancy needs neither corrupted coins nor CH5-1b's evo/effect hooks. Both nodes can even return the same SKIPPED verification result if their disconnected prefixes differ but neither finishes the requested range.

Preserving every fill for reads that actually execute closes R2's independent omission, but cannot make the executed read sets equal when E4-1 permits different coverage. Thus “membership is not part of this escalation” is too strong. The broad reference to cache effects “once CH5-1b lands” does not accurately disclose this existing CH5-1a consequence.

**Fix:** add parent-fill/membership, subsequent ATMP reject reason, P2P orphan/parent-request and subsequent uncache/cache-history consequences to E4-1's proposed allowed-diff scope whenever the verification ranges differ. Qualify CH5-1a's no-intervening-flush equality claim by equal executed read coverage and corresponding initial membership. Keep the R2 guarantee that corresponding reads preserve corresponding fills. Add a seam case with different permitted L3 prefixes and a cold T output to pin this consequence. No change of counter or reinstatement of a Core-model meter is requested.

## R2 source feasibility check

The relevant rdc paths are in `/work/home/agent/workspace/rdc-wt/integration/dashd-chainstate/src/coins/`:

- `cache.rs:202–205, 248–277`: `fetch`/`fetch_bytes` first respects active/F, including spent entries. A DB hit is inserted into active with `Flags::NONE`; a missing coin returns without insertion. `have_in_cache` observes active/F and excludes spent entries. These are the membership behaviors needed from Core's RPC parent `FetchCoin`/`HaveCoinInCache`.
- `cache.rs:209–223`: the current below-cache readers reach prefetch/DB directly. A production scratch parent adapter must route these misses to the attached tip's fetching read. In particular, covering only mutable `fetch_bytes` is insufficient: `overlay.rs:210–218, 245–250` also uses the shared-read `peek_below` path. Rev2's requirement of **every** scratch miss covers both paths.
- This needs an actual scratch-parent extension: the current cache constructor accepts a guarded engine, not another cache (`cache.rs:146–148, 162–184`). It is nevertheless feasible within the existing engine type: a parent handle can fetch under the tip's exclusive guard and copy the returned packed bytes into scratch-owned storage before releasing the guard. No borrowed tip bytes need escape across a later tip mutation. This also preserves `ScratchCoins`' current `CoinsCache<E>` engine-type requirement (`chain/seams.rs:741–758`).

Existing active/F entries need not become newly clean entries; they retain their existing flags. Prefetch/DB hits become clean entries, and misses remain absent. Scratch mutations must stay in scratch; clean parent-read insertions and prefetch consumption are the necessary exception to the older blanket “no writes ... entries ... prefetch” wording at PLAN.md:63. The explicit new parent-fill requirement controls that implementation, rather than suppressing the fetch's native bookkeeping.

These checks establish plan feasibility, not the existence or correctness of the future production adapter. The required process comparisons and membership assertions remain implementation acceptance gates.

## New findings introduced by rev2

No additional blocker or major beyond the remaining R1 disclosure issue above. R9's design-note gate is assessed as a gate, without demanding that its future note already exist.

## Validation and limits

Read REVIEW-COMMON.md, diffed rev1 against rev2, and read both preceding review files. Checked Core behavior with `git show v24.0.0-rc.5:<path>` and traced the rdc cache/overlay/scratch seams read-only. This is an unimplemented plan review; no production scratch adapter or copy protocol exists here to run, and no future process-test success is claimed. No source changes, builds, commits, pushes or GitHub actions were performed. Only this requested scratch review file was written; managed chain, benchmark, cache and restricted-tree contents were not accessed.

## Verdict

**APPROVE-WITH-CHANGES** — amend E4-1's observable scope and the associated membership claim before treating R1 as closed. R2, R9, N1 and N2 are closed at plan level.
