# SB0 narrow confirmation review, round 7 — Sol (DEC-92)

Reviewed `docs/node-program/designs/SB0-shared-base.md` **rev6 at f03c86a0**, against **rev5 at 1f75dbb6** and `reviews/SB0-design-r6-sol.md`. All SB0 line references below identify f03c86a0. Followed `briefs/REVIEW-COMMON.md`; read the program context and binding DEC-115/116, with DEC-104/105/107/109/112/113 retained. This review confirms the four r6 closures and checks the changed contracts; it does not reopen settled sections.

**Result: APPROVE. Sol gate satisfied: yes.** R6-M1, R6-M2, R6-M3 and R6-m1 are closed. No blocker, major, minor or nit remains in this confirmation scope, and no new gap was found in the rev5→rev6 changes.

## R6-M1 — Closed; the refusal does not wedge SPV

**Refs:** `docs/node-program/designs/SB0-shared-base.md:442`, `:452`, `:517`; SB4 `:1570`, SB5 `:1571`.

The candidate now passes both the failed/conflicting-ancestry walk and descent from the floor. Core's `FindMostWorkChain` checks the path to the active chain (`v24 validation.cpp:3533–3576`); Core's marking can leave stale descendants unflagged (`:4112–4122`). Rev6 correctly adopts the eligibility check without copying the candidate walk's status propagation. `best_header` and its Core acceptance, walk-back and load rules remain those of rev5.

The three-branch witness now selects A100. The unflagged C105 is excluded by C81 and by its ancestry at the floor height. The floor condition independently handles the already-active lock where Core marks nothing. Restart and cold-record variants, plus the published-tip property test, are assigned to SB4/SB5.

The `set_active_tip` refusal is a backstop for an incorrectly supplied target, not the fork-choice mechanism. Under the specified ChainLock safety assumption, a known floor is itself an accepted eligible header; an unknown locked header does not advance the floor (`SB0:520`). Thus SPV can choose the floor even when it has less work than the previous tip. Extensions descending from it remain eligible. The header task recomputes the target against the new floor, and the lock plus final tip change form one header step. The SPV publication contract (`:740`, `:746`, `:1343–1351`) does not require publishing the intermediate common ancestor below the new floor. Ordinary execution therefore does not repeatedly select a refused target. A cold-read failure follows the existing fail-stop contract rather than publishing an incompatible tip.

## R6-M2 — Closed; the safety assumption is acceptable within the recovery contract

**Refs:** `docs/node-program/designs/SB0-shared-base.md:1368–1380`; SB13 `:1580`, SB14 `:1581`.

The unknown-hash branch now performs one truncate and rescan from the durable floor, capped by wallet height and durable tip, or from the anchor without a floor. It no longer treats the durable tip as a common fork point. The A100/A70 → Buffered B110 witness rewinds to 70; the straight-extension control uses the same conservative rule. Both are assigned alongside the independent-database crash checks.

The proof uses **ChainLock safety as an assumption**: verified locks do not conflict. It does not claim signature verification itself proves that assumption. The cited Core `UpdateBestChainlock` advances on a higher height and assigns the known block index without checking ancestry against the previous lock (`v24 chainlock/chainlock.cpp:106–129`). Relying on the same safety property for this bounded recovery argument is acceptable.

The floor must be the one in the **surviving SPV WAL prefix at open**. Header records, statuses, active projection and applied floor are committed together; every accepted branch record is retained; a Durable write covers earlier commits (`SB0:686`, `:1310–1311`, `:1343–1360`). Consequently an unknown wallet hash was published after that prefix. Its publication floor was the surviving floor or a later, non-conflicting descendant. Both chains therefore contain the surviving floor and its ancestors.

A non-conflicting higher lock can reorganize blocks **above** the previous floor, but it cannot strand H in the unknown-hash case:

- If the higher lock is in the lost suffix, recovery still uses the earlier durable floor, common to both branches.
- If a higher lock became Durable **after** the wallet's branch was published, the earlier header step became Durable too. The wallet hash is then known, including on a stale branch, and startup uses the actual fork instead of the unknown-hash rule. For example, A120 can lock a branch diverging from wallet B110 at A80 while descending from A70; preserving that later lock also preserves B110's header record.

The immutable anchor supplies the common point when no floor survives. The argument is bounded by the stated commit-before-publish and retained-prefix recovery contracts; it does not substitute a newly learned floor for the recovered one or authorize runtime re-anchoring. These bounds are sufficient for the requested failure case.

## R6-M3 — Closed under DEC-116

**Refs:** `docs/node-program/designs/SB0-shared-base.md:970–986`, `:1005–1037`; SB11 `:1577`, SB15 `:1582`.

Null-base diffs contain the active commitments at their targets (`v24 evo/smldiff.cpp:31–64`). Core takes the last `signingActiveQuorumCount` mined commitments for a non-rotated type, or the last commitment per index for a rotated type (`llmq/blockprocessor.cpp:818–832`). Thus the stated cycles-active rule is correct; failed DKG rounds lengthen lifetimes rather than shorten them.

The mining endpoints are inclusive (`blockprocessor.cpp:319–323`). The subtraction is the maximum endpoint displacement, **end − start**, not the number of allowed mining heights. A window 10…18 has nine permitted heights but displacement eight. Mining the predecessor at the latest endpoint and its replacement at the earliest gives these minimums:

| Network / served type | Arithmetic | Minimum | Compared with W_P = 38 |
|---|---|---|---|
| mainnet / 100_67 | 24 × 24 − (18 − 10) | 568 | holds |
| testnet / 25_67 | 24 × 24 − (18 − 10) | 568 | holds |
| devnet / devnet_platform | 24 × 4 − (18 − 10) | 88 | holds |
| regtest / test_platform | 24 × 2 − (18 − 10) | 40 | holds; margin 2 |

These agree with `v24 llmq/params.h:249–268`, `:327–346`, `:462–509` and each network's Platform-role assignment in `chainparams.cpp`. The informational table entries also agree, including 400_85's displacement 28 and the rotated regtest type's minimum 16. DEC-116 correctly excludes that latter type from the post-prune service invariant; stock regtest is not rejected. The params test and configuration refusal apply to the configured served Platform type and its spacing-derived W_P, as specified.

With minimum lifetime greater than W_P, a commitment authorized at h either existed at h−W_P and was not replaced then, or was mined later and cannot yet have been replaced at h. Conversely, membership at either target satisfies the authorization predicate. Two authenticated targets therefore suffice. The replaced-at-r boundaries are correct: h = r+W_P−1 obtains Q at r−1; h = r+W_P sees Q at neither target. Rows, the LRU and successful fetches use that same predicate. Verification inputs, below-anchor unavailability, deduplication and failure behavior remain explicit. SB11/SB15 exercise the boundaries after pruning and restart. Other types' post-prune not-found behavior is the allowed difference in DEC-116.

## R6-m1 — Closed; CPM reproduced

**Ref:** `docs/node-program/designs/SB0-shared-base.md:1674`.

The corrected prose agrees with the rerun: M2 / REL11 / M5 are **18.5 / 30.0 / 41.5** for A, B and stress, and **21.5 / 31.0 / 42.5** for C. SB12a is 11.5 and SB12 is 14.0 in A/B/C; SB14 remains 20.5/18.0/23.0. The stress run gives SB14 22.0, SB15/SB16 26.0, SB17 28.0 and SB18 28.5. No model edge or size changed in rev6.

## Verification and limits

- Read the pinned v24 source for best-header updates, candidate ancestry checks, ChainLock marking and update, active-quorum selection, mining windows, parameters, network roles and diff construction. The reconciler's saved `params.h`, `blockprocessor.cpp` and `chainparams.cpp` match the tag byte for byte.
- Reran the existing `/work/scratch/sb0-reconcile/rev6/r6.py` **on a scratch copy of rev5**. It reproduces rev6, including the lifetime table, apart from the final corrected `ChainError::BelowAnchor` citation. That directory contains a document generator rather than a separate executable arithmetic checker; independently parsed all 14 pinned parameter entries and checked the table arithmetic and served-role mapping.
- Ran `TMPDIR=/dev/shm/sb0-review-r7-sol-tmp PYTHONDONTWRITEBYTECODE=1 python3 /work/scratch/sb0-review-r7-sol/checks.py`: **8 tests passed**. They check endpoint arithmetic, two-target equivalence and pruned lookup boundaries, three-branch and no-marking choice, refusal and forward progress, reopened SQLite cold records, independent wallet-database rewind/rescan, anchor fallback, and higher-lock ordering. These are specification models. Saved allowed Durable prefixes model suffix loss; they do not simulate physical power failure or execute the future SB4/SB11/SB14 code or BLS verification.
- Reran `docs/node-program/tools/cpm.py` and `/work/scratch/sb0-reconcile/rev5/cpm_sb0_rev5.py` against that committed model, which is unchanged from f03c86a0. All run-specific numbers above reproduce. `git diff --check 1f75dbb6 f03c86a0 -- docs/node-program/designs/SB0-shared-base.md` passed.
- The changes preserve the previously accepted contracts and DEC-104/105/107/109/112/113. Existing consumer coordination and implementation gates remain requirements; this confirms the design, not their execution. No source/build change requires a broad build rerun.
- Followed PLAN §7 handling (CANDIDATE-SPV): codenames only. No excluded/private material was investigated. No live node was started or queried; no commit, push or GitHub action was taken. This requested report is uncommitted. Review-owned scratch databases/files and the mode-700 TMPDIR were removed; other agents' files were left untouched.

## Verdict

**APPROVE. Sol gate satisfied: yes.** All four r6 findings are closed within the binding rulings. Proceed to the sequential Opus confirmation required by DEC-112.
