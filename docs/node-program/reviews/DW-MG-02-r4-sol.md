# DW-MG-02 governance facade — independent review, round 4

Reviewed detached `166c66b4b70b8586b02e65af47f4b49520dce4dc` against `008146de`, the round-3 findings, and D76. The branch under review was not edited.

## Verdict: CHANGES REQUIRED

D71 closes round 3's per-key-cap case, and D72 closes the failed D64 refetch case while their uncertainty records remain. D76 introduces two paths to `Synced` with a definite wrong tally: forged `notfound` answers can erase a lost vote after peer disconnections, and lost-vote refetches can use up an honest peer's announcement allowance so its next real vote is counted as synced but never fetched.

## Findings

### Major — Disconnected peers' `notfound`s can settle a lost vote with only one peer connected

`rust/crates/dw-governance/src/sync.rs:998-1011,1396-1405,1429-1480,1604-1629,2021-2044`; `rust/crates/dw-governance/src/store.rs:1271-1305,1368-1383`; `rust/crates/dw-engine/src/governance/reads.rs:361-369`

Use the round-3 NO example: A has 101 exact YES weight against 100, and Core counts an honest NO of weight 4, leaving 97. A listed key holder first re-signs that NO's fields, so its copy hides the honest copy by hash, then sends a newer ABSTAIN in the same unresolved slot. D76 correctly records the removed NO as lost. Two peers controlled by the key holder answer `notfound` to the refetch-only request while a third, honest peer holds the NO. After those two answers, the honest peer's request is pending. Let one answering peer disconnect, then the honest peer disconnect before serving its copy. `on_closed` calls `retry`; `none` still contains both disconnected/connected answerers, `live` now contains only the one remaining answerer, and `none.len() >= 2 && live.iter().all(...)` settles the NO. The newer fake ABSTAIN cannot lower net yes, so A becomes definite Passing with net yes 101 and no close-call flag, although Core's honest vote makes it 97.

The request is phaseless (`Taken::default()`), and giving it up while `refetching` does not invalidate completed vote syncs. This can occur after the run has already published `Synced`; it can also leave the votes phase free to publish `Synced` using its completed sync history. The claim that a lost vote stays uncertain with one connected peer is true only when that peer was alone throughout the request. More generally, two peers' unauthenticated `notfound`s cannot prove that a previously observed vote does not exist on a disconnected honest node.

A temporary regression using the existing `lose_honest_no` fixture, two `notfound`s, and the two disconnects passed: the store's `uncounted` changed from 1 to 0 with one peer still connected. Keep the placeholder until a valid replacement/superseding vote or proposal expiry, or retain a close-call marker if peer absence is used as a practical stopping rule. Counting only peers still connected at settlement would repair the one-peer claim but would not make two forged answers proof that Core has no vote.

### Major — Synthetic refetch announcements can suppress a genuine vote while completing its sync

`rust/crates/dw-governance/src/sync.rs:448-468,1033-1104,1216-1243,1604-1629,1799-1805,2021-2044`; `rust/crates/dw-governance/src/store.rs:113-120,1271-1305,1739-1815`

`refetch_votes` adds every connected peer to every lost hash's `announced.all` and increments each `PeerState.announced`, even though none of those peers sent an `inv`. `on_inv` attributes a known vote hash to that peer's proposal before checking `state.announced >= max_announcements`; at the limit it neither asks for nor stores the vote. The attribute can satisfy `votes_completed`, while the synthetic refetch requests have `phase = false` and do not hold the votes phase open. A previously decoded, rejected copy is enough to put the genuine vote's parent in `vote_parents`; it need not create uncertainty in the store.

The production limits permit this: 100 proposals can each retain 1,000 lost hashes, below the 5,000-object cap, and that fills the default 100,000 announcement allowance of **every** connected peer. One listed key can create the lost hashes by sending successively newer signed votes in one unresolved slot per proposal. Those replacements are kept, so the sender's 25,000-*unkept* budget does not stop them. An honest peer's next announced YES at A (for example, 99 exact YES plus its real weight 4) can then be skipped while its `ssc` count is credited, yielding `Synced` and definite nonpassing 99 instead of Core's 103.

A temporary reduced-limit regression (`max_announcements = 1`) passed: one lost-vote refetch consumed the honest peer's sole allowance, its genuine known-parent `inv` made `votes_completed` true, no `getdata` was pending for that vote, and the store still had zero YES. Keep refetch bookkeeping separate from peer-originated announcement allowances, and do not count a discarded phase vote `inv` as a completed sync without a retained copy or bounded uncertainty.

## Round-3 closure and D76 costs

| Item | Round-4 ruling |
|---|---|
| M5, ABSTAIN fillers crowding out a genuine YES | **Closed on the direct path.** `place` records `KeyLimit` network votes by key, outcome and hash; `tally` widens the outcome and `build_table` marks any uncounted vote a close call. The 99/100 and 50/100 row tests and the three-peer sync regression cover it. The two findings above can still produce a false definite row through D76's separate paths. |
| D64, failed wrong-key refetch | **Closed on the direct path.** `set_resolved` keeps an awaited vote; exhausted refetch leaves it uncertain, and a reconnect cannot turn it definite. The awaited record is not settled by `notfound`. The named row and reconnect tests cover the r3 NO example. |
| D73, object-only full-sync history | **Closed.** Insertion now prunes expired full-sync entries independently of vote syncs; the named object-only test covers it. |
| D76 retained bounds | The store's held refused/lost hashes share 1,000 per proposal; overflow saturates lost outcomes to whole-list weight and keeps a close-call marker. Unknown-object lost entries are trimmed after 20,000 queue entries to at most 10,000 live entries. I found no unbounded retained map or endless phase from these paths. The per-run refetch work can nevertheless reach 5,000 × 1,000 hashes and bypasses the 100,000 peer announcement guard (second finding). |
| D76 accepted availability costs | Same-time copies that peers still serve but the store rejects can keep a proposal close until expiry; a run with exactly one peer throughout cannot settle its lost vote from one `notfound`. These are conservative. The documented claim that two answers mean no node holds the vote, and the resulting one-peer claim across disconnects, are not sound (first finding). Refetch timeouts have finite phase deadlines; I found no new honest-peer suspect charge in this path. |

## Validation

- `cargo test -p dw-governance --lib`: **PASS**, 186 tests including the two temporary regressions.
- The temporary regressions were written only in the detached review checkout and are not proposed branch changes. The owner-reported desktop, regtest, and mutant gates were read from the brief, not rerun.

## Open questions or assumptions

The first schedule assumes the two `notfound` peers can lie and the honest holder can disconnect before its refetch turn; both are permitted by the current peer handling. The second requires enough signed replacements to reach the production allowance; the code imposes per-proposal and object bounds, but no lower aggregate refetch cap.
