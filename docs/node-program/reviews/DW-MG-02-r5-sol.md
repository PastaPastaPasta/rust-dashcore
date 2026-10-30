# DW-MG-02 governance facade — Sol review, round 5

Reviewed `166c66b4..255d0dcc` in a detached checkout at `255d0dcc8d1bbb164816ff12c5094fb59b937d61`. The branch under review was not edited.

## Finding

### Minor — The refetch budget can permanently starve later hashes

`rust/crates/dw-governance/src/store.rs:1393-1402`; `rust/crates/dw-governance/src/sync.rs:1672-1678`

`requeue_lost` walks the same unchanged `HashMap`s on each run. `refetch_votes` takes the entire queue, starts only its first 1,000 hashes, and drops the rest. If those first hashes remain lost because no peer serves them, they take the budget again on every run. A later awaited or lost hash can therefore go unasked until the earlier proposals expire or are pruned, even when a peer would serve its copy. This is the recorded D78 N2, but I rate persistent starvation a Minor availability issue rather than a Nit. Rotate the starting point, or retain a bounded cursor/queue across runs, so a deferred hash eventually gets a turn. The affected rows stay close calls; the budget does not make a tally definite or hold a sync phase open.

## Round-4 Majors

| Item | Round-5 status |
|---|---|
| Forged `notfound`s plus disconnections cleared a lost NO | **Closed.** The settlement call is gone. `on_not_found` only advances a separate refetch (`sync.rs:1416-1449`); the retained lost record is removed by a stored copy or a strictly newer stored vote of the outpoint's resolved voting key (`store.rs:1434-1465`, `1176-1224`). The exact three-peer `lose_honest_no` schedule remains uncounted after two `notfound`s and the disconnects. |
| Synthetic refetches consumed the honest peer's announcement allowance and credited a dropped vote sync | **Closed.** Initial refetches and retries use `Refetches`, not `Requests::announced` or `PeerState::announced` (`sync.rs:1665-1737`). An over-limit phase vote `inv` increments only `unattributable`, not `vote_announced` (`sync.rs:1083-1111`). With `max_announcements = 1`, the honest next vote is requested; a separately dropped vote completes no sync. |

## D77/D78 checks

- **DEC-257 resolved-key rule:** `settle_uncounted` drops the exact stored hash and calls `supersede_lost` only when the stored key equals the resolved voting key; `set_resolved` looks up the stored vote under that same key. `supersede_lost` retains equal-time votes. The remover's newer vote leaves the lost NO uncertain while the outpoint is unresolved. The earlier unstored-object FIFO can forget a record before proposal storage, as already designed in D76; I found no new path in this range from that exception to a definite wrong row.
- **Refetch separation and bound:** `refetches.started` increases once per initially asked hash and gates every `refetch_votes` start at the default 1,000. Peer retries reuse that hash's start. `notfound`, refusal, timeout and close only move it to the next live peer. Refetch requests are not phase requests and are absent from `announced.all`, `vote_announced`, and `votes_completed`. Exhaustion leaves retained records uncertain; the phase still has its idle and overall deadlines. The fairness finding above is the remaining cost.
- **D78 credit:** A pre-sync relay marked held is credited only if `has_vote` still holds it at the later ask (`sync.rs:1875-1893`). `on_vote` marks every peer's relay held after a copy lands (`sync.rs:1333-1343`), covering the fetched-orphan-then-expired case. Awaited hashes are requeued (`store.rs:1382-1402`). I found no new regression in this rule. The existing D36 treatment of relays the store never held remains outside this fix range.
- **Other recorded nits:** One-item retry `getdata` (`sync.rs:1722-1737`), a redundant `getdata` when a normal request stores the same vote (`sync.rs:1267-1275`, `1713-1718`), refetch refusals charging the sender's `unkept` counter (`sync.rs:1344-1351`), and always asking peer 0 first (`sync.rs:1688-1700`) remain Nits. They add bounded work or affect which peer pays an existing per-run limit; none clears uncertainty or completes a sync. The fixed-order starvation above is more than a Nit.

## Validation

- `CARGO_TARGET_DIR=/work/scratch/dw-mg02-r5-sol/target TMPDIR=/work/scratch/dw-mg02-r5-sol/tmp CARGO_BUILD_JOBS=12 cargo test -p dw-governance --lib -- --test-threads=4` — **PASS, 192/192**. This includes `forged_notfounds_and_disconnects_leave_a_lost_vote_uncounted`, `a_refetch_takes_no_announcement_allowance`, `a_dropped_phase_vote_completes_no_sync`, the budget and asked-peer tests, and the D78 held/gone tests.
- No regtest, testnet, UI, or live oracle suite was run, per the brief. The owner-reported larger gates were not independently rerun.

VERDICT: ACCEPT-WITH-CHANGES
