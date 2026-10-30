# DP2-08 independent review r2 — fix round f1

Reviewed `2d4ba8c2..5423cbd7` in `dw/dp2-08-add-contact`, read-only. This review addresses only the three r1 findings and faults introduced by this delta.

## Finding

1. **Major — A late initial list response can still turn a fresh `ignored` hit into Send.** `apps/desktop/src/contacts/add.ts:80-84`, `apps/desktop/src/screens/contacts/ContactsPage.tsx:305`, `apps/desktop/src/contacts/queries.ts:28-34,89-94`. `listed.at` is TanStack's `dataUpdatedAt`, assigned when the response enters the cache, not when the list snapshot was read. Consider an initial contacts query that reads an empty pre-ignore list and remains in flight. The sender's request arrives and is ignored; a subsequent search or verified scan returns `ignored`. The ignore's invalidation occurs while that initial list query has **no cached data**. In TanStack Query 5.104.1, `cancelRefetch` only cancels an in-flight query when it already has data; this invalidation reuses the old promise and starts no replacement read. When the old empty response arrives after the ignored hit, its `dataUpdatedAt` is newer than the hit's `readAt`. `currentRelation` therefore returns `stranger`, and an `ok` eligibility answer can offer Send to the ignored sender. A read-only QueryClient/QueryObserver probe against the installed version showed one read before invalidation and still one read after the stale response. The supplied tests only give the stale list an *older* timestamp, so they miss this case. **Suggested fix:** require a list fetch known to have begun after the ignored relation was obtained before allowing it to clear `ignored`; alternatively keep `ignored` until an explicit unignore or authoritative relation check. Add a regression where the initial list promise starts before the ignore, is invalidated with no cached data, and resolves after the ignored hit.

   The timestamp ordering also fails if the system wall clock moves backward: TanStack's `dataUpdatedAt` and the scan's `Date.now()` can make an older cached list appear newer than a fresh ignored relation. The same fix should use a monotonic ordering or explicit query generation rather than comparing wall-clock values.

## Other r1 findings

- **Oversized dropped or pasted files: closed.** `apps/desktop/src/contacts/add.ts:317-329` checks `file.size` before `arrayBuffer()`, checks the resulting buffer again, and maps a failed read to a user-facing refusal. `AddContact.tsx:386-398` sends files through this path. The boundary and unreadable-file tests cover the new behavior. No delta fault found.
- **Aging auto-accept claim: closed within the stated 30-second wake limit.** `apps/desktop/src/contacts/add.ts:151-186` schedules a tick at the 60-second margin, removes the note, and replaces Send with the expiry line. `AddContact.tsx:159-166,276-289` wires the timer to the card. The documented delay after system sleep remains a minor UI limit with an engine expiry check; it does not by itself require changes in this narrow review. No other delta fault found.

## Checks

I inspected the delta and the installed TanStack Query 5.104.1 implementation. I ran only the read-only QueryClient probe described above; I did not rerun the owner's build or test suite. The branch remained untouched.

VERDICT: CHANGES-REQUESTED
