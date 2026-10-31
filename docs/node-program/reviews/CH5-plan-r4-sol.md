(Durable copy of /work/scratch/ch5/reviews/plan-r4-sol.md; wording changed only where the docs commit gate requires it.)

# CH5 rev3 narrow re-check

**Scope:** only the remaining R1 from `plan-r3-sol.md`, its R2 implementation notes, and new major/blocker issues introduced by the rev2 → rev3 diff. The memory-counter choice remains the manager's decision.

## R1 — major — CLOSED

Rev3 closes the remaining disclosure and acceptance gap at plan level:

- **E4-1, `/work/scratch/ch5/PLAN.md:390–400`:** consequence 6 explicitly places coins-cache membership differences in CH5-1a, separately from CH5-1b's evo effects. It includes `HaveCoinInCache`, ATMP reject reasons, parent requests/orphan retention, and subsequent uncache/cache history and flush timing. It also covers different disconnected prefixes when both calls return SKIPPED. The proposed ALLOWED-DIFFS scope now includes items 1–6.
- **§3 membership sentence, `PLAN.md:374`:** the former exclusion of membership is replaced with preservation for reads that actually run and an explicit reference to the different-range consequence. Read in this escalation's scope, it no longer promises that retaining corresponding parent fills makes different verification ranges membership-equivalent.
- **CH5-1a scope 1, `PLAN.md:60–68`:** equality is expressly conditional on the same executed scratch-read sequence and corresponding initial membership, without an intervening flush. The implementation notes cover both mutable `fetch_bytes` and shared `peek_below`, require fetching under the tip guard and copying bytes into scratch-owned storage, preserve active/F flags, promote prefetch/DB hits to clean entries, and leave misses absent. They explicitly sanction the resulting clean parent fills and prefetch consumption. BL-278(b)'s exception refers to that described bookkeeping, so it no longer contradicts the parent-fetch requirement. Scratch coin mutations remain isolated.
- **New seam case, `PLAN.md:121`:** different permitted L3 prefixes with a cold output of T explicitly pin consequence 6. This supplies the requested plan-level acceptance case; its future implementation must assert the described membership and downstream behavioral difference rather than infer equality from equal SKIPPED results.

The concrete scenario remains consistent with Core v24.0.0-rc.5: disconnecting T's block spends/checks its outputs through the scratch view (`src/validation.cpp:2342–2349`), whose fetch reaches and fills the RPC tip parent (`src/coins.cpp:44–65,129–149`; `src/rpc/blockchain.cpp:1438–1440`). Skipping that block need not fill the output. With spent inputs, cached-output membership selects `txn-already-known` instead of `bad-txns-inputs-missingorspent` (`src/validation.cpp:972–990`), and only the latter enters the missing-input parent/orphan branch (`src/net_processing.cpp:4976–5010`). Startup's parent is the DB instead (`src/node/chainstate.cpp:436–440`), as the plan distinguishes.

The R2 implementation notes are feasible against the read-only rdc integration source: `coins/cache.rs:202–223,248–277` establishes the existing active/F, prefetch/DB and membership behavior; `coins/overlay.rs:210–218,245–250` confirms both below-cache paths require the parent extension. Copying under the guard avoids exposing borrowed tip bytes, while retaining the `CoinsCache<E>` shape required by `chain/seams.rs:741–758`. This is feasibility evidence, not approval of an adapter that has yet to be implemented.

## New findings from the diff

No new major or blocker. No changes requested in this narrow re-check.

## Validation and limits

Read `REVIEW-COMMON.md` and `plan-r3-sol.md`, diffed `PLAN-rev2.md` against `PLAN.md`, and checked the relevant Core sources using `git show v24.0.0-rc.5:<path>` plus the rdc integration cache/overlay/seam sources. This is an unimplemented plan review: no runtime test can validate the future adapter or new seam case yet, and no such test success is claimed. No repository changes, builds, commits, pushes or GitHub actions were performed. Only this requested scratch review file was written; managed chain, benchmark, cache contents and restricted trees were not accessed.

## Verdict

**APPROVE** — R1 is CLOSED at plan level. The manager's counter decision and future implementation acceptance gates remain outstanding outside this narrow review.
