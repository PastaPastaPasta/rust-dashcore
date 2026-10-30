# DW DP1-09 narrow r2 review at afd6b556

## Result

Both r1 findings are fixed. I found no blocking issue in the three-commit fix delta or the follow-merge.

- **P1:** `fundingStep` has one caller, `RepairModel.resolveStuck`. It now passes an orphan's complete `registration/<n>/funding` ID through unchanged and still constructs that ID for a normal row. The engine emits the orphan with that complete `draft` and an empty `label`; Repair displays a clear fallback instead of an empty list item. The new unit test checks the exact step sent to `resolveStuckFunding`.
- **P2:** For an idle failed row in `Standing::Awaiting`, `registration.rs` calls `idle_wait`, which returns exactly `FundingUnknown`, `Network`, or `ChainLock`. The fixture now retains those three waits and clears other waits. Its new test covers all three retained values and two cleared values. The R4 e2e test pins the failed/unknown scenario through Home's `creatingRepair` card, the registration repair screen, Tools ▸ Repair's stuck row, confirmation, settlement, and removal of that row. The orphan step path is pinned by the Repair unit test, not by R4.
- **Follow-merge:** `git show --remerge-diff 2ac55376` shows one conflict, in `fixture.test.ts`; the resolution retains both main's `payContact.test.ts` and DP1-09's `registration.test.ts`/`registration.ts`. The remaining BL-165, DP3-06, and MG-01 changes merged without a manual conflict; comparison against both parents showed no dropped main addition. Main had m4 item 51 and Contract-Version 25; the merged tree retains DP1-09 items 52–53 and Contract-Version 27.

**Documentation nit:** `docs/contracts/m4-dashpay-engine.md` item 53 still says a failed row's wait is `None` (around line 2790). That contradicts the same contract's §2.2 `Awaiting` rule (around line 424), the engine, and new D32. Qualify the sentence with the failed `Awaiting` exception when next editing m4. This wording predates the r2 fix and does not affect the implementation or numbering.

## Checks

- `pnpm exec vitest run src/tools/repair.test.ts src/fixture/registration.test.ts`: 2 files, 33 tests passed.
- `git diff --check 2ac55376 afd6b556` and both parent-to-merge diff checks: clean.
- R4 e2e code inspected; browser test not rerun. The worktree remained clean and was not edited.

VERDICT: ACCEPT-WITH-NITS
