# DW-MG-02 governance facade — independent review, round 3

Reviewed detached `008146ded8c95a98a0eb36b105cbf166ba6962bd` against `9d1fa28f`, the MG-02 design and Contract-Version 3, my round-2 findings, and Dash Core `v24.0.0-rc.3`. The branch under review was not edited.

## Verdict: CHANGES REQUIRED

M1–M4 and M5's specific forged-YES case are closed. M5 still has a current, reachable false-definite-status path through the unresolved per-key cap: signed fake ABSTAIN votes can crowd out a genuine YES vote while the phase reports `Synced`. D64's future resolution path has a separate false-definite-status case when refetch fails. An object-only session also retains an unbounded full-sync history.

## Findings

### Major — Unresolved key-limit fillers can hide a genuine vote while reporting Synced

`rust/crates/dw-governance/src/store.rs:1318-1349`; `rust/crates/dw-governance/src/sync.rs:449-457,1044-1058,1212-1226,1280-1324,1984-2005`; `rust/crates/dw-governance/src/tally.rs:163-205,213-295`; `rust/crates/dw-engine/src/governance/reads.rs:318-361`

Take A at threshold 100 with 99 exact YES weight, ample budget, and a Core-valid evonode YES vote V of weight 4 whose listed voting key K has one masternode (`n_K = 1`). Core counts V, so A's net yes is 103. In an earlier connection, the holder of K sends the wallet four funding ABSTAIN votes on invented outpoints for A; the store carries them into the next run. Core rejects those outpoints, but the wallet cannot resolve them and accepts the four votes: they fill K's `2*n_K+2` unresolved slots. They affect the wallet's abstain range, not its net-yes range.

When three honest peers announce and deliver V, the wallet's `place` rejects every copy as `KeyLimit`; its hash never enters `seen_votes`. `on_vote` has already decoded V and `credit_relays` has attributed its inv to A for those peers. `KeyLimit` is a refusal that is retried but never makes a lost phase item, so all three `ssc(11,1)` syncs complete and the run publishes `Synced`. The row stays at exact net yes 99 with no close-call flag, despite Core's 103. The existing `key_limit_fillers_charge_no_honest_announcer` test reproduces the filler and honest-copy sequence but checks only that honest announcers avoid a suspect charge; it never checks the missing vote, sync result, or row.

Keep the rejected, decoded funding vote as bounded uncertainty for its proposal, or fail the vote phase visibly when this cap can discard a vote that Core may count. A regression should use ABSTAIN fillers, honest YES copies from the vote-sync peers, and assert that `Synced` cannot coexist with a definite nonpassing row.

### Major — Failed D64 refetch can turn an honest NO into definite Passing

`rust/crates/dw-governance/src/store.rs:985-1015,1027-1061`; `rust/crates/dw-governance/src/sync.rs:449-457,988-1001,1044-1058,1578-1600,1614-1680,1984-2005,2028-2032`; `rust/crates/dw-engine/src/governance/reads.rs:318-361`

Take current proposal A at threshold 100 with 101 exact YES weight, ample budget, and an honest evonode NO vote V of weight 4 at outpoint O. Core validates V against O's voting key and has net yes 97, so A is not passing. A different listed key holder re-signs V's fields and sends that copy first. The vote hash excludes the signature, so the wallet stores the wrong-key copy under V's hash while honest peers' later invs of V are already known. Before O is resolved, D64 correctly gives that NO copy a 0..4 range and marks A a close call.

Resolve O to its actual voting key while the run is `Synced`. `set_resolved` queues V for refetch and deletes the wrong-key copy, including its only 0..4 uncertainty. Let the honest peers disconnect before refetch, and have the remaining peer answer `notfound`. The refetch is exhausted without a vote; because it is marked `refetching`, `retry` does not set `vote_given_up`, and the session retains `Synced` and its completed vote-sync history. `build_table` now sees exact net yes 101, reports `Passing`, and leaves `close_call = false`. If those honest peers reconnect inside the repeat window, `ask_votes` cannot ask them again; `short_vote_syncs` still counts their earlier completions, so the new run can also publish `Synced` without V. A peer's old inv count is not evidence that a vote the store has since deleted was recovered.

Keep a 0..4 missing-copy placeholder for V (with its NO outcome and parent, which its hash authenticates), or make the proposal's sync stale and revoke completions when the copy is dropped. Do not report a definite status from the reduced tally until a valid replacement is fetched or the phase visibly fails. Add a test with an honest NO and exhausted refetch that checks the row and a reconnect, not only the `vote_given_up` flag.

The named `a_listed_key_signing_for_another_outpoint_makes_no_pass` test covers a forged unresolved YES before resolution. `a_resolved_outpoint_brings_back_the_honest_copy` covers successful refetch. `a_refetch_given_up_leaves_vote_syncs_recorded` explicitly covers exhausted refetch but asserts only that the history flag stays clear; none checks the resulting false definite row.

`set_resolved` is public but has no production caller in MG-02. This second finding becomes user-visible when MG-03 supplies outpoints; the first finding is reachable in MG-02 now.

### Moderate — Full-sync history grows for life when there are no proposals

`rust/crates/dw-governance/src/sync.rs:301-333,1950-1961`

Start with an empty current-proposal set, which is an ordinary governance state. Repeated runs meet different reachable peer addresses as the masternode list or configured peers change; a peer that leaves can make each run retry. Sending each full `govsync` inserts an address into `full_synced`, including a run that fails before completion. With no proposal, `ask_votes` never calls `note_votes_synced`, the **only** path that prunes `full_synced`. After an hour, the old entries stop blocking requests but stay allocated. Repeat over the session's lifetime and the map grows with every distinct peer address, despite D59's statement that the history is pruned with the vote record. This needs many peer identities and is slower than M1's two-peer stream, but it is still an unbounded peer-keyed retained map.

Prune expired full-sync entries when adding one, independently of vote asks. Preserve live repeat-window entries so an eviction cannot cause a penalized repeat request. A test should run object-only syncs across distinct addresses with a short test window and assert that expired addresses leave the history without any proposal vote sync.

## Round-2 closure and narrow regressions

| Item | Round-3 ruling |
|---|---|
| M1, expired tombstones | **Closed.** `check_object` refuses a lapsed proposal on arrival; `prune` removes a stored one at its end epoch. `expired` and `erased` plus their FIFO orders each have fixed 10,000-entry caps, and `has_object` checks them before fetching. The scaled batch test exercises arrival and expiry. Forgotten hashes can be fetched and refused again, but retained state and prune work stay bounded. |
| M2, greedy skips | **Closed.** D67's funded rule charges every candidate that might rank above, even one the greedy loop might skip; its unfunded rule charges only candidates proven funded and surely above. Both are conservative and move monotonically to a fixpoint. Exact counts resolve in rank order, including skips. The named three-candidate test uses my A/B/C numbers; the property test enumerates every count combination in 3,000 small cases. I found no exact-count false close call or unsound funded verdict in the new algorithm. |
| M3, retry inside repeat window | **Closed for the original H1/H2/M schedule.** Sent but unfinished H1/H2 syncs are excluded from `vote_completed`; one fresh withholder's `ssc(11,0)` cannot meet the two-peer floor. The test seeds that first-run history and observes `Failed`. A completed history becomes stale if D64 later deletes its vote (finding above). |
| M4, unrelated unknown inv and parent clear | **Closed.** Unknown inv plus `notfound` can release a phase but cannot complete A; the one-peer test reaches `Failed`. `vote_parents` forgets one oldest entry at a time; losing attribution makes a sync short rather than completed. The FIFO test checks the cap, though it does not replay the end-to-end post-eviction schedule. |
| M5, listed key forging another outpoint | **Partly closed.** A stored unresolved claim is 0..4; the named forged YES at 99/100 is a close call, not a definite pass. Four fake ABSTAIN claims can instead fill one key's unresolved slots and exclude its genuine YES with no net-yes uncertainty (first Major finding). The resolution/refetch path separately reopens a definite wrong answer (second Major finding). |
| Moderate, lapsed lifecycle and fundable membership | **Closed as an explicit wallet rule.** D65/D69 choose current proposals for status, close calls and allocation, test that one set, and document Core's different lapsed retention and two-hour payment fudge window. The old helper-parity claim is gone. |

The new `expired`, `erased`, and `vote_parents` maps and FIFO orders are capped, as are the run's unresolved votes and the existing store/request maps. Their eviction cannot turn an unknown vote into a completed sync. The remaining retained-state leak is the object-only `full_synced` path above. The correctness state losses are a valid vote refused at `KeyLimit` without uncertainty and a D64 vote deletion without uncertainty or completion invalidation. With the D68 floor, one available honest peer after a two-peer failure can remain `Failed` until fresh peers or the repeat window are available; that is a finite availability cost of avoiding Core's repeat penalty, not a false `Synced` result.

## Validation

- `cargo test -p dw-governance`: **PASS** — 164 unit tests, 3 integration tests, 4 golden tests, no failures. All named governance regressions in this crate passed.
- `cargo test -p dw-engine governance`: **PASS** — 4 matching tests, no failures.
- `cargo test -p dw-protx --test no_operator_signing`: **PASS** — 5 tests, no failures.

Reviewed the named regression tests and the Dash Core tag's `CGovernanceVote::IsValid` outpoint/key check. No live-network writes were made.
