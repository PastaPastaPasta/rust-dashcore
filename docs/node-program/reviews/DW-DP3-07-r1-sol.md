# Independent review: DW DP3-07 at 03113555

Reviewed `75b0d9b6907f95d08b4f406d4cf46eeaf0d01059...0311355583ddca36b1f00396658d3a9086be0dbc` without editing the worktree.

## Findings

1. **P2 — A labeled contact payment has no accessible counterparty name** — `apps/desktop/src/components/history.tsx:71-80`. The avatar's initials are `aria-hidden` in `Avatar`, and a transaction label wins the row title. For the fixture's “Coffee refund” payment, the visible `BB` avatar represents Bob Builder, but the row button's accessible name contains the label, time, and amount without Bob's name. A screen reader user cannot get the counterparty information carried by that avatar until opening the detail. D1's rationale says the title names the contact for screen readers, but that is true only when there is no label. Add the contact name as visually hidden row text (or an equivalent accessible label) when a label takes the title, and assert the labeled row's accessible name in Playwright. This affects both Home and Activity list rows.

2. **P3 — The new interaction tests leave several stated branches unpinned** — `apps/desktop/e2e/history-contacts.spec.ts:66-135`. The positive “View contact” path uses an ordinary contact; the negative path uses only a stranger. No test changes the main identity, opens a hidden or ignored contact, or checks the focus fallback when that contact's row is collapsed. The tests click rows and buttons with a pointer, so they do not establish D5's keyboard flow. D8 checks Home's discreet state but not the lock transition. The implementation appears to handle these cases through the owner-keyed unfiltered list, the sheet focus fallback, and the lock overlay; add focused cases so the branch's tests protect those claims. The fixture's Marta row is a useful proxy for a counterparty absent from the main identity's list, but it does not model a second identity's ownership of the payment.

## Checks and assessment

- `pnpm build:fixture` passed, including token check, TypeScript, Vite build, and fixture bundle guard.
- `./node_modules/.bin/vitest run src/fixture/historyContacts.test.ts src/format/tx.test.ts` passed: 17 tests in 2 files.
- `pnpm exec playwright test e2e/history-contacts.spec.ts --project=chromium --workers=2` passed: 13 tests, including the new serious-axe checks in light and dark themes.
- `git diff --check 75b0d9b6...03113555` passed. The reviewed worktree remained clean.
- Inspected the committed light and dark bell PNGs from the exact base and head at their original 1280×820 size. Both use the `dashpay-registered` fixture. The new Home strip appears behind the unchanged bell popover and pushes History down; the popover content remains the same. The re-baseline matches the intended layout change. I did not rerun the opt-in pixel comparison, which is pinned to the Playwright image.

The row and detail use the engine-provided `TxCounterparty` rather than guessing from names or addresses. “View contact” is gated by the main identity's owner-keyed, unfiltered contact list, including its hidden section; an unknown counterparty gets no button. Home unmounts its frequent strip in discreet mode, while the existing lock screen covers the inert shell. I found no UI path here that substitutes another identity's contact in a history row. The pre-existing transaction detail selects `rows[0]` for multi-record transactions, so the documented F5 limitation remains.

VERDICT: CHANGES-REQUESTED
