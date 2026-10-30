# DP2-09 review r2 (Sol)

Reviewed `git diff db17e999 fba78ead` for the three r1 findings and new faults in the F1 delta (`015ec8ba..fba78ead`). The branch was left unchanged.

## Findings

None.

## R1 closure

- **Bell ownership:** `Toolbar` now keeps the wallet and main identity that opened the bell, and renders the popover only while that owner still matches (`apps/desktop/src/components/shell.tsx:218-226,254`). `App` supplies the owner from the displayed wallet and identity (`apps/desktop/src/app/App.tsx:190-220`). A keyboard wallet switch therefore unmounts the old popover without mounting the new wallet's or marking its events read. D27 covers this with separate unread journals.
- **Focus after Accept:** `useRefocus` waits for a successful action's old row or pane action to leave, then focuses a surviving request, pane actions area, or heading if focus fell to the body (`apps/desktop/src/screens/contacts/afterAction.ts:43-71`). Distinct pane button keys prevent React from retaining focus on a reused disabled Pay button (`apps/desktop/src/screens/contacts/ContactPane.tsx:203-232`). D28 exercises pane, Contacts rows, popover and page with keyboard actions.
- **Outcome after a wallet switch:** `useActionToast` checks the wallet currently displayed when the action settles and prefixes a toast with the originating wallet's name if different (`apps/desktop/src/screens/contacts/afterAction.ts:14-37`). The hook is also used by the pane, request rows, banner, private details, popover and page, so an outcome whose original view disappeared remains visible. D27 covers a held Accept settled after a wallet switch.

## Known limitations and checks

- I agree D30's **L3** can stay open: an incoming request inserted above the clicked request can shift the stored index before refocus (`apps/desktop/src/screens/notifications/Notifications.tsx:204-215`, `apps/desktop/src/screens/contacts/ContactsPage.tsx:170-173`). The focus still lands on a request or heading; it may choose a different request than the one formerly adjacent. A future change can resolve the target from current request identity and position instead of the click-time index.
- I agree D30's **L4** can stay open: two fixture paths use the first wallet's DashPay state for receive usernames and stuck funding (`apps/desktop/src/fixture/send.ts:689-698`, `apps/desktop/src/fixture/tools.ts:475-479`). They do not participate in the D27 bell and Accept paths. They should use the wallet-specific state before the two-wallet fixture is reused for receive or registration tests.
- I relied on the brief's UI gate (Vitest 653; Playwright and axe 728 passed, 170 skipped after the allowed timeout rerun), merge typecheck and Vitest results, and unchanged Rust/Cargo/CI trees. I did not rerun the gate or testnet GL.

VERDICT: ACCEPT
