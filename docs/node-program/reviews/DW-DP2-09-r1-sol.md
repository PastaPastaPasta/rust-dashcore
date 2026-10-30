# DP2-09 review r1 (Sol)

Reviewed `53a78fb1..015ec8ba` against D1–D26. The branch was left unchanged.

## Findings

1. **Major — Switching wallets with the bell open can mark the new wallet's events read without opening its bell.** `apps/desktop/src/components/shell.tsx:213` keeps `bellOpen` across wallet changes, while `:220` and `:248` render whichever wallet's `bellPanel` is currently available. A keyboard user can change the wallet picker while the popover is open; the outside-click handler does not run. When the new wallet's status and identity load, its popover mounts automatically, and `apps/desktop/src/notifications/queries.ts:33-40` marks its first page read. Its unread badge can disappear even though the user never opened that wallet's notifications. Bind the open state to the wallet and identity that opened it, or close it when either changes. Add a keyboard wallet-switch test with unread events in both wallets.

2. **Minor — Focus is lost after an accepted request removes its button.** `apps/desktop/src/screens/notifications/Notifications.tsx:203-212` and `apps/desktop/src/screens/contacts/ContactPane.tsx:193-203` render Accept only while the relation is pending. Acceptance removes that focused control, and neither surface moves focus to a surviving request, row, heading, or outcome. The grant sheet may briefly return focus to Accept, but that button then disappears. A keyboard user must find their place again; the current tests assert the relation and outcome but not focus after completion. Move focus to a stable target after the row changes, and cover the pane, popover, and page with keyboard tests.

3. **Minor — A completed action can announce success for the previous wallet on the current wallet's screen.** `apps/desktop/src/screens/notifications/Notifications.tsx:40-50` sends every result from an unmounted view to the global toast without checking the action's wallet. If Accept is submitted in wallet A, the user switches to wallet B while the Platform call is in flight, and the call completes, wallet B displays “You and X are now contacts.” The write is correctly addressed to A, but the toast gives the wrong context. Keep D23's completion notice while naming the originating wallet, or otherwise scope it to that wallet; test a wallet switch after grant submission.

## Checks and assumptions

- The accept model's per-surface busy guard works. In a detached scratch worktree, 23 targeted Vitest tests passed. Replacing `if (this.running) return null` with an inert guard made `runs one action at a time` fail as expected; the scratch worktree was then removed.
- An ignored request cannot be accepted later through a stale view: the platform-wallet dependency drops the pending incoming entry under the same identity write lock used by Accept, so Accept refuses it. I did not run the testnet GL or repeat the full gate; the brief's gate and GL evidence remain the basis for those results.
- The banner uses the main identity from `DashPay.status` and shows only when that identity's `hasDashpayKeys` is explicitly false. The search invalidation uses the wallet and identity prefix. The shared avatar rule applies only to the Contacts and Notifications components inspected here.

VERDICT: CHANGES-REQUESTED
