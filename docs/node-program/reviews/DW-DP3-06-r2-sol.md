# DP3-06 narrow delta review — Sol r2

Reviewed `b378ec4e` on `dw/dp3-06-pay-contact` at `3bbd9753`, against the r1 findings. This was a read-only static review; I did not run tests under the reported high load. `git show --cc 3bbd9753` has no combined diff, so the follow-up merge contains no conflict resolution.

## Findings

None.

## Checks

- **r1 Low #1 resolved.** `apps/desktop/src/screens/pay/ResultSheet.tsx:50-62,72` increments `data-retries` once in the retry's `finally`, after `draft.broadcast` settles. The result component stays mounted when `PayToContact.tsx:356-364` replaces one result outcome with another, so the count survives unknown-to-unknown retries. `apps/desktop/e2e-tauri/pay-contact.ts:224-238` reads the count before pressing Try again and waits for it to rise, or for a sent/failed result. A stale unknown result still has the old count and cannot satisfy that wait. The new fixture case at `apps/desktop/e2e/pay-contact.spec.ts:196-213` covers two immediate unknown answers followed by sent.
- **r1 Low #2 resolved.** `apps/desktop/src/screens/pay/ConfirmSheet.tsx:212-245` joins the acceptance sentence to the refusal without adding another “not sent” clause. The mapped refusal strings in `apps/desktop/src/screens/pay/model.ts:142-231` are standalone sentences, including `send.no_peers`, `send.broadcast_rejected`, and `send.cancelled`; the C8 assertion was updated at `apps/desktop/e2e/pay-contact.spec.ts:112-120`.
- **Payment safety unchanged.** `ResultSheet.tsx:53-58` retries `draft.broadcast` with the same draft and prepared handle carried by the unknown outcome. This delta adds no prepare, authorize, or new-payment path. The GL ledger remains written before Send and refuses an unfinished earlier run (`apps/desktop/e2e-tauri/pay-contact.ts:212-215`; existing `earlier` guard).

## Assumptions and limits

The GL runs the renderer built from this head, so its result sheet has the `data-retries` marker. The review covers the specified delta and merge check; it does not re-review the mainline BL-162 changes or claim a live testnet run.

VERDICT: ACCEPT
