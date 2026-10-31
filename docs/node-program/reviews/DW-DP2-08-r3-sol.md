# DP2-08 independent review r3 — fix round f2

Reviewed `5423cbd7..419cb934` on `dw/dp2-08-add-contact`, read-only. Scope: the Major from r2 and faults introduced by this delta.

## Findings

None.

The r2 Major is closed. `contactListQuery` takes its sequence before calling `dashpayContacts`, while `networkSearchQuery` and verified scans take theirs after their relation answers arrive. Thus an initial list read begun before an `ignored` hit keeps its lower sequence even when its response lands later; `currentRelation` retains `ignored`. The same sequence avoids wall-clock rollback. `refreshAfter` resets active, data-less first reads before invalidation, so an action also starts a new list or search read instead of inheriting TanStack's running promise. The real-QueryClient regression covers the late initial response and the reset behavior.

I checked the possible post-hit stale-list case against the backend path: search and scan relations use `relations_to_main`, which reads the same wallet contact state as `dashpayContacts`; a live read replaces the stored snapshot under the wallet read guard, and a read during a writer uses that snapshot until a later signal. A list request begun after an `ignored` answer therefore cannot use an older pre-ignore snapshot on this path. A list begun between a relation's backend read and its response may be conservatively treated as older, which can delay clearing `ignored`, but cannot expose Send for an ignored sender. Unignore refreshes both contacts and search. The `contactsAll` reset also matches detail and activity first reads, but only while they have no data; active queries refetch and populated queries retain their displayed data.

I inspected the delta, query and action call sites, and the backend contact-state path. I did not rerun the owner's reported typecheck, Vitest, or browser suites. `git diff --check` passed; the branch was untouched.

VERDICT: ACCEPT
