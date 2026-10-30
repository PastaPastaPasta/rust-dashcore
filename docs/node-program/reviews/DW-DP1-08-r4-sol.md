# DP1-08 independent narrow review r4 — f3, 1da4fe97

## Findings

None in the requested scope.

## R3 finding closure

- **D31 (Major): closed.** `NamesChecks::order` prioritizes identities whose question has never completed, and `answered` stamps questions that answered, failed, or reached their own timeout. A question cut off by the check budget remains unstamped. For a finite identity set, even if a slow question ends just inside its 10-second limit, completed questions move behind the ninth identity on the next check. Adding identities does not reset an existing identity's stamp or generation; a sustained stream of new identities faster than the four-at-a-time budget cannot have a bounded service guarantee, but does not reproduce the fixed-prefix starvation reported in r3.
- **D32 (Minor): closed.** `NamesRuns::start` tests `active` and `epoch` and spawns into the `JoinSet` while holding the same mutex that `stop` uses to disarm and abort. There is no gate-to-spawn gap. `set_retry` uses the same epoch gate, and a retry carrying the old epoch is rejected even after a later start rearms checks.
- **D33 (Minor): closed.** Rollback calls `fence_names` after `remove_from_manager` completes, including its error path. That second generation bump rejects an answer from a check that started after the first forget but read the wallet before removal. Restore and ordinary import hold `wallet_lifecycle`, so a legitimate re-import cannot cross the second fence.

I reviewed the immutable `aa9881c5..1da4fe97` delta and the related lifecycle paths. The three new regression tests and the supplied final test evidence support these closures; I did not build or run tests. No branch files were edited. Assumption: the identity set is finite during any retry sequence, and public re-import follows the wallet lifecycle lock.

VERDICT: ACCEPT
