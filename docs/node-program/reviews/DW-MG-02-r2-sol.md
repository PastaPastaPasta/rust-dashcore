# DW-MG-02 governance facade — independent review, round 2

Reviewed `dw/mg-02-governance` at `9d1fa28f6798cd61eac0a65791caf46c86cb8eb3` in a detached worktree, against `docs/design/MG-02-governance.md`, `docs/contracts/mg-engine.md`, my round-1 findings, and Dash Core `v24.0.0-rc.3`. The reviewed source was not changed.

## Verdict: CHANGES REQUIRED

The inbound bound, request-kind retry, shared-key ownership display, and concurrent object cap fixes are present. Two round-1 correctness findings are still open in narrower forms, and the object store still has an unbounded retained-state path. I would not merge while a peer can drive an unbounded tombstone map or while the UI can report a definite budget result that the documented vote ranges can reverse.

## Findings

### Major — Expired-proposal tombstones bypass the total store bound

`rust/crates/dw-governance/src/store.rs:269,609-655,771-818`; `rust/crates/dw-governance/src/validate.rs:257-313`; `rust/crates/dw-governance/src/sync.rs:978-1072,1454-1527`

Two controlled peers announce the same stream of distinct, syntactically valid proposals whose `end_epoch` is already more than ten minutes past. The wallet does not check collateral; two announcements make each object confirmed. `check_proposal` accepts the old end time. Each one is inserted below the 5,000-object / 64 MiB cap and, on the next one-second `prune`, removed from `objects` and inserted into `erased` with `until = i64::MAX`. The next batch again fits the object cap. Neither the lifetime announcement count nor the rolling 16 MiB inbound budget limits the stream. `erased` has no cap and `prune` scans it with `retain` every tick, so memory and periodic CPU work grow for the session's lifetime. This preserves the resource-exhaustion part of round-1 Major 2 despite the new live-object cap.

Reject an already expired proposal before storing it, or bound the tombstones and use an expiry-aware re-fetch policy. A regression should run several batches of unique old proposals through confirmation and prune, then assert a finite retained-state bound.

### Major — Greedy budget skips defeat the D48 close-call extremes

`rust/crates/dw-governance/src/tally.rs:325-343,382-416`; `rust/crates/dw-engine/src/governance/reads.rs:313-358`

The estimate, all-low, all-high, and each candidate's one-against-all extreme do not cover greedy allocation with skips. With threshold 10 and budget 14, take hashes ordered `A < B < C`:

| Proposal | Cost | Net yes low | Estimate | High |
|---|---:|---:|---:|---:|
| A | 8 | 14 | 23 | 23 |
| B | 9 | 18 | 18 | 21 |
| C | 6 | 20 | 20 | 23 |

The estimate funds A and C. All-low and all-high also fund A and C. A's tested worst case (`A=14, B=21, C=23`) funds C, skips B for lack of room, then funds A. The implementation therefore leaves `A.close_call = false`. The feasible mixed endpoints `A=14, B=21, C=20` fund B first and then skip C and A; A is unfunded. These ranges can be made entirely from exact yes votes plus independent, unresolved yes votes under mixed-type shared keys: A has three `(1,4,4)` claims atop 11 exact votes, B one `(1,1,4)` claim atop 17, and C one `(1,1,4)` claim atop 19. Adding the same exact votes to all three meets a higher network threshold without changing the order. Dash Core's `GetApprovedProposals` score order and `getFundableProposalHashes` greedy budget loop at this tag have the same skip behavior. Round-1 Major 6 is therefore not closed.

Use a sound over-approximation for each candidate's membership under the permitted ranges; it may mark extra close calls. Add this three-proposal case beside the opposite-endpoint test.

### Major — A vote-sync retry inside the repeat window can display `Synced` with missing votes

`rust/crates/dw-governance/src/sync.rs:315-330,1545-1587,1770-1782,1816-1845`

A first run sends `govsync(A)` to honest H1 and H2, then its connections fail before A's vote arrives. The send records H1 and H2 in `vote_synced`, even though neither completed the request. A retry within the one-hour mainnet window connects H1, H2, and a new withholding peer M. `ask_votes` excludes H1/H2 and sends only to M. M replies `ssc(11, 0)`; `asked > 0`, so `unassigned` remains empty, every peer is `votes_done`, and the run publishes `Synced` without A's vote. The store carried no copy from the failed run. This is the D56 admitted schedule, but its outcome violates the phase's stated completeness condition. A visible incomplete or stale phase until a usable peer can be asked is safer than asserting `Synced`; it does not require making a penalized repeat request.

### Major — Unknown vote invs can still settle an unrelated `ssc(11)`; clearing `vote_parents` discards known attribution

`rust/crates/dw-governance/src/sync.rs:435-459,960-966,1010-1024,1159-1170,1314-1341`

While M owes one vote on proposal A, it may send `ssc(11, 1)`, an `inv` for an unknown vote V on unrelated B, and then `notfound(V)`. The inv goes into `unresolved`; the completed retry removes it and increments the peer-wide `unattributable` counter. `votes_done` spends that counter against A's shortfall, with no decoded A vote and no pending request. This is the round-1 Major 4 failure mode for an undecoded relay. It is most consequential in the repeat-window schedule above, where M is the only peer asked for A.

The `vote_parents` map is also cleared wholesale at 100,000 entries. A peer can fill it with sequential requested, decoded non-funding votes that `precheck` ignores without penalty; their announcement slots are released after each answer. An earlier decoded but unkept B vote then loses its parent record (and can age out of the bounded store `ignored` map). A later inv of that known B vote from M is treated as unknown, and `notfound` can credit A as above. The rolling inbound budget bounds queued bytes, not total messages over a run. Do not credit a vote with no proven parent to a specific proposal's `ssc`; if the parent-history bound is exhausted, fail the phase or retain the active syncs' attribution instead of clearing all credit.

### Major — One listed voting key can forge definite tally weight for another outpoint

`rust/crates/dw-governance/src/store.rs:943-949,1216-1273`; `rust/crates/dw-governance/src/tally.rs:223-303`; `rust/crates/dw-governance/src/sync.rs:1173-1286`

For an unresolved collateral outpoint, `key_ok` accepts a signature from **any** voting key in the masternode list. A holder of one evonode key can sign a YES funding vote naming an unrelated or nonexistent masternode outpoint and have one peer announce and deliver it. The first such claim is below the `2*n_K+2` limit. The tally attributes four votes with a `[4,4]` weight range and increments no `ranged_votes`, while Dash Core `CGovernanceVote::IsValid` rejects the vote because `GetMNByCollateral(outpoint)` is absent or has another voting key (`src/governance/vote.cpp:231-238` at `v24.0.0-rc.3`). A proposal at net yes 99 against a threshold of 100 can appear passing at 103, with no close-call signal. A re-signed copy of an honest vote arriving first has the same hash and can also suppress the honest signature while changing its weight. The D59 per-key limit bounds the size of the distortion, but does not make the displayed result sound. This is a design-level limitation of unresolved outpoints, not a newly introduced parser bug; the contract's definite tally and budget status need either outpoint verification or a conservative unverified-claim treatment before merge.

### Moderate — Expired proposal lifecycle and fundable membership differ from Core

`rust/crates/dw-governance/src/store.rs:787-817`; `rust/crates/dw-engine/src/governance/reads.rs:313-327`

The design says Core erases a proposal ten minutes after `end_epoch`; the wallet does. In Core `v24.0.0-rc.3`, `CGovernanceObject::IsValidLocally` deliberately calls `ValidateProposal(..., fCheckExpiration=false)`, and the only `SetExpired()` call in `src` is for triggers in `superblock.cpp:504`. A lapsed proposal can remain in `mapObjects` until a delete vote, while `GetApprovedProposals` and `getFundableProposalHashes` do not filter its end time. For example, a still-stored lapsed proposal A with the highest yes count and a full-budget payment is considered before a current proposal B by Core's helper; the wallet removes A and reports B as fundable. Core's actual trigger builder does check the payment time window, so copying the helper may be undesirable; the implementation and its “Core helper parity” claim need one explicit, tested rule.

## Round-1 closure

| Round-1 Major | Round-2 ruling |
|---|---|
| 1. Unbounded inbound queues | **Closed.** `subscribe_bounded` carries frame credits through the event channel, with 64 frames, 1,024 events and a per-peer 16 MiB outstanding-byte budget (`dw-p2p/session.rs:473-485,591-613`; `dw-governance/peer.rs:219-247`). |
| 2. Confirmed objects bypass all caps | **Partly closed.** Live objects have 5,000 / 64 MiB total caps, but expired confirmed proposal hashes accumulate without a bound (finding 1). |
| 3. `notfound` changes request kind | **Closed.** Requests are keyed by `(hash, kind)` and a mistyped reply retries the original kind (`sync.rs:1314-1341`). |
| 4. Unrelated relay settles vote sync | **Partly closed.** Decoded and stored votes use parent attribution; undecoded/given-up votes and the wholesale `vote_parents` clear can still settle an unrelated count (finding 4). |
| 5. Shared-key vote shown as each holder's | **Closed.** Shared-key rows use `key_last_vote`; an own vote needs a resolved outpoint (`dw-engine/governance/voting.rs:53-82,158-176`). |
| 6. Opposite ranges change budget order | **Not closed.** D48 covers the two-proposal case but misses greedy skips among three proposals (finding 2). |

## Rulings on the owner's open items

| Item | Ruling |
|---|---|
| D51: one key pins 5,000 fake objects | **Acceptable for this slice as a visible availability failure**, with the store-cap fix above. It needs a valid voting key plus confirming peers; the run fails rather than claiming completeness. Revisit when proposal submission must protect wallet-owned objects. |
| D56: fresh withholding peers inside repeat window | **Blocks merge.** A routine failed run can turn into a false `Synced` on a retry; finding 3. |
| D59: re-signed copy below key limit | **Blocks merge.** The key limit still permits definite but false vote weight and budget status; finding 5. |
| D59: orphan slot unlimited until parent | **Acceptable as a memory bound.** The store caps orphans at 2,000 per sender and 50,000 total (`store.rs:63-67,1276-1314`); the forged-claim issue above remains. |
| D59: first deliverer's unverified collateral txid | **Acceptable for display-only MG-02.** A malicious first copy can change it because it is outside the object hash; document it as peer-reported, and do not use it as payment proof in a later flow. |
| D63: peer past 25,000 unkept recoveries loses a keepable vote | **Acceptable as a visible failed phase.** The limit is finite and does not turn a timed-out item into `Synced`. |
| D62/D63: one recovery past 100,000 remembered entries | **Acceptable.** The per-peer 25,000 recovery budget makes the excess finite for a run. |
| D47: wallet-owned object may be evicted | **Acceptable for MG-02**, which submits no object; the submission slice must protect its own object before release. |
| `vote_parents` cleared when full | **Blocks merge.** It can erase the only known parent of a decoded, unkept vote, enabling the unrelated-count schedule in finding 4. |
| `make_room` sorts on every full-store insert | **Acceptable as a bounded performance risk for this slice.** It is expensive under a sustained two-peer flood and merits a heap/index or benchmark, but the unbounded tombstone map is the demonstrated resource blocker. |
| MockPeer `chatter` option never fires | **Acceptable test-helper nit.** It does not affect production; tests relying on that option need correction. |

## Core parity and guard

The weighted tally and threshold follow Core's 1/4 weights and `max(minQuorum, validWeight/10)` for genuine votes; estimate ordering and hash tie-break also match Core. The interval close-call computation is unsound for greedy skips (finding 2), unresolved vote claims can corrupt weights (finding 5), and expired proposals differ from Core's helper (finding 6). `funded_height` uses the same height rule as Core's `getProposalFundedHeight` for triggers it retains. Core gets its triggers from the active set; the facade uses two-peer confirmation because it cannot verify the operator signature or chain execution. This documented SPV limitation should be visible when describing a “Funded” row.

The operator-signing surface remains a stub in `dw-engine/governance/voting.rs:142-155`. `rust/clippy.toml` still disallows the BLS secret-key and signer APIs; the `no_operator_signing` guard result is recorded below.

## Validation

- `cargo test -p dw-governance -p dw-p2p --quiet`: **PASS** (152 governance unit tests, integration targets with 3 and 4 tests, and 14 P2P unit tests; no failures).
- `cargo test -p dw-engine governance --quiet`: **PASS** (both governance facade unit tests, including the shared-key regression; other targets had no matching test names).
- `cargo test -p dw-engine --test mg_governance_contract --quiet`: **PASS** (9 tests).
- `cargo test -p dw-protx --test no_operator_signing --quiet`: **PASS** (5 tests). This checks the no-operator-signing guard alongside the banned APIs in `rust/clippy.toml`.
- Independently enumerated a three-candidate greedy-budget schedule using Core's score and skip order; the exact estimates, bounds and mixed outcome appear in finding 2. Reviewed Dash Core source at the requested `v24.0.0-rc.3` tag. I did not repeat the manager's four-masternode regtest or gate 18.
